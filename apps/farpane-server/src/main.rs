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

#[cfg(windows)]
mod service;

use farpane_server::{
    backend::{self, EventLog, Level, LiveBackend, LiveInfo},
    config::{self, TlsChoice},
    dashboard::{Dashboard, Page},
    settings::{ServerSettings, TlsMode},
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
  --desktop              share the desktop of the session this runs in: the Wayland one
                         if $WAYLAND_DISPLAY is set, else the X11 one ($DISPLAY)
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
  --max-connections N    most connections at once, handshakes included (default 256)
  --ws ADDR              also accept WebSocket connections (noVNC) on ADDR; needs TLS
                         unless ADDR is on this computer (loopback)
  --ws-origin ORIGIN     web page allowed to use the WebSocket port (repeatable, `*` for
                         any; default: pages served from the same host)
  --h264                 offer H.264 to clients that ask for it (needs `farpane-server h264 install`)
  --require-approval     hold each new client until you allow it in the dashboard
  --view-only            ignore keyboard, pointer and clipboard input
  --ui-screenshot FILE   save a picture of the dashboard and quit
  --ui-delay SECONDS     wait this long before that picture (default 1.5)
  --theme NAME           start with glass, slate, light or system (default glass)
  --page NAME            start on overview, clients, users, security, displays or log

Settings saved by the dashboard's Security page (server.toml in the config
folder) apply unless an option here says otherwise.

H.264 (--h264, or the Security page) needs Cisco's OpenH264 library, which is
not part of Farpane: `farpane-server h264 install` downloads it from Cisco.

Users (farpane-server user add NAME) sign in with name and password inside
TLS and have a role: admin, control or view. Passwords are never taken from
the command line.";

#[allow(clippy::struct_excessive_bools)] // independent command-line switches
#[derive(Clone)]
struct Args {
    listen: SocketAddr,
    size: (u16, u16),
    fps: u32,
    max_connections: Option<usize>,
    theme: Option<farpane_ui::ThemePreference>,
    require_approval: bool,
    /// Offer H.264 to clients that ask for it.
    h264: bool,
    /// Also accept WebSocket connections (browsers) here.
    ws: Option<SocketAddr>,
    ws_given: bool,
    /// Web pages allowed to connect: origins, or `*` for any.
    ws_origins: Vec<String>,
    password: Option<Vec<u8>>,
    view_only: bool,
    allow_no_auth: bool,
    x11: bool,
    wayland: bool,
    /// Share the desktop of the session this runs in.
    desktop: bool,
    x11_display: Option<String>,
    config_dir: Option<PathBuf>,
    self_signed: bool,
    ra2: bool,
    /// `--listen` was given (so a saved address does not replace it).
    listen_given: bool,
    headless: bool,
    ui_screenshot: Option<PathBuf>,
    page: Page,
    ui_delay: f64,
    tls_names: Vec<String>,
    tls_cert: Option<PathBuf>,
    tls_key: Option<PathBuf>,
}

type Words = std::iter::Peekable<Box<dyn Iterator<Item = String>>>;

fn value(it: &mut Words, name: &str) -> Result<String, String> {
    it.next().ok_or(format!("{name} needs a value"))
}

fn parse() -> Result<Args, String> {
    parse_from(std::env::args().skip(1))
}

#[allow(clippy::too_many_lines)] // one arm per option
fn parse_from(words: impl Iterator<Item = String> + 'static) -> Result<Args, String> {
    let mut args = Args {
        listen: "127.0.0.1:5900"
            .parse()
            .map_err(|_| "bad default address")?,
        size: (1024, 768),
        fps: 30,
        max_connections: None,
        theme: None,
        require_approval: false,
        h264: false,
        ws: None,
        ws_given: false,
        ws_origins: Vec::new(),
        password: std::env::var("FARPANE_PASSWORD")
            .ok()
            .filter(|p| !p.is_empty())
            .map(String::into_bytes),
        view_only: false,
        allow_no_auth: false,
        x11: false,
        wayland: false,
        desktop: false,
        x11_display: None,
        config_dir: None,
        self_signed: false,
        ra2: false,
        listen_given: false,
        headless: false,
        ui_screenshot: None,
        page: Page::Overview,
        ui_delay: 1.5,
        tls_names: Vec::new(),
        tls_cert: None,
        tls_key: None,
    };
    let mut it: Words = (Box::new(words) as Box<dyn Iterator<Item = String>>).peekable();
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
            "--require-approval" => args.require_approval = true,
            "--h264" => args.h264 = true,
            "--ws" => {
                args.ws = Some(
                    value(&mut it, "--ws")?
                        .parse()
                        .map_err(|_| "--ws: an address and port, such as 0.0.0.0:5800")?,
                );
                args.ws_given = true;
            }
            "--ws-origin" => args.ws_origins.push(value(&mut it, "--ws-origin")?),
            "--theme" => {
                use farpane_ui::ThemePreference as T;
                let name = value(&mut it, "--theme")?;
                args.theme = Some(match name.to_ascii_lowercase().as_str() {
                    "glass" => T::Glass,
                    "slate" => T::Slate,
                    "light" => T::Light,
                    "system" => T::System,
                    _ => return Err("--theme: glass, slate, light or system".into()),
                });
            }
            "--page" => {
                let name = value(&mut it, "--page")?;
                args.page = Page::parse(&name)
                    .ok_or("--page: overview, clients, users, security, displays or log")?;
            }
            "--listen" => {
                args.listen = value(&mut it, "--listen")?
                    .parse()
                    .map_err(|e| format!("--listen: {e}"))?;
                args.listen_given = true;
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
            "--max-connections" => {
                args.max_connections = Some(
                    value(&mut it, "--max-connections")?
                        .parse()
                        .map_err(|_| "--max-connections: not a number")?,
                );
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
            "--desktop" => args.desktop = true,
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

/// Add what the dashboard saved in `server.toml`; options given on the command
/// line win.
fn with_saved_settings(args: Args) -> Result<Args, String> {
    let config_dir = args
        .config_dir
        .clone()
        .unwrap_or_else(config::default_config_dir);
    let saved = ServerSettings::load(&config_dir.join("server.toml")).map_err(|e| e.to_string())?;
    merge_settings(args, &saved)
}

/// The command line with the saved settings under it: what the command line
/// gives wins, the saved settings fill in the rest.
fn merge_settings(mut args: Args, saved: &ServerSettings) -> Result<Args, String> {
    let saved = saved.clone();
    if !args.listen_given
        && let Some(listen) = saved.listen
    {
        args.listen = listen
            .parse()
            .map_err(|e| format!("server.toml: listen: {e}"))?;
    }
    if saved.tls == TlsMode::SelfSigned && args.tls_cert.is_none() && args.tls_key.is_none() {
        args.self_signed = true;
    }
    args.ra2 |= saved.ra2;
    args.view_only |= saved.view_only;
    args.require_approval |= saved.require_approval;
    args.h264 |= saved.h264;
    if !args.ws_given {
        args.ws = match &saved.websocket {
            Some(ws) => Some(
                ws.parse()
                    .map_err(|e| format!("server.toml: websocket: {e}"))?,
            ),
            None => None,
        };
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
    if let Ok(p) = std::env::var("FARPANE_PASSWORD")
        && !p.is_empty()
    {
        return Ok(p);
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
    #[cfg(windows)]
    service::write_log(line);
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
        Event::Denied { peer, user, reason } => format!(
            "{} was turned away{}: {reason}",
            who(peer),
            user.as_ref()
                .map_or_else(String::new, |u| format!(" ({u:?})"))
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
    let mut args = args.clone();
    if args.desktop && !args.wayland && !args.x11 {
        if std::env::var_os("WAYLAND_DISPLAY").is_some() {
            args.wayland = true;
        } else if std::env::var_os("DISPLAY").is_some() {
            args.x11 = true;
        } else if cfg!(windows) {
            return Err(
                "--desktop: sharing the Windows screen is not built yet (only the test \
                 pattern is available)"
                    .to_owned(),
            );
        } else {
            return Err("--desktop: neither $WAYLAND_DISPLAY nor $DISPLAY is set".to_owned());
        }
    }
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

/// The H.264 codec, when it is switched on: Cisco's `OpenH264` library from the
/// config folder.
fn video_codec(
    args: &Args,
    config_dir: &std::path::Path,
) -> Result<Option<Arc<dyn rfb_codec::video::VideoCodec>>, String> {
    if !args.h264 {
        return Ok(None);
    }
    let dir = farpane_h264::library_dir(config_dir);
    match farpane_h264::OpenH264::open(&dir).map_err(|e| e.to_string())? {
        Some(codec) => Ok(Some(Arc::new(codec))),
        None => Err(format!(
            "H.264 needs Cisco's OpenH264 library, which is not in {}. Download it with \
             `farpane-server h264 install`.",
            dir.display()
        )),
    }
}

/// `farpane-server h264 install | status [--config-dir DIR]`.
fn h264_command() -> ExitCode {
    let mut words: Vec<String> = std::env::args().skip(2).collect();
    let config_dir = match words.iter().position(|w| w == "--config-dir") {
        Some(i) if i + 1 < words.len() => {
            words.remove(i);
            PathBuf::from(words.remove(i))
        }
        _ => config::default_config_dir(),
    };
    let dir = farpane_h264::library_dir(&config_dir);
    match words.first().map(String::as_str) {
        Some("status") => {
            match farpane_h264::OpenH264::open(&dir) {
                Ok(Some(_)) => println!("OpenH264 is installed in {}", dir.display()),
                Ok(None) => println!(
                    "OpenH264 is not installed (it would go in {}): run `farpane-server h264 install`",
                    dir.display()
                ),
                Err(e) => println!("OpenH264 is in {} but {e}", dir.display()),
            }
            ExitCode::SUCCESS
        }
        Some("install") => {
            println!("{}", farpane_h264::NOTICE);
            match farpane_h264::install(&dir) {
                Ok(path) => {
                    println!("Installed {}", path.display());
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("farpane-server: {e}");
                    ExitCode::FAILURE
                }
            }
        }
        _ => {
            eprintln!("Usage: farpane-server h264 install | status [--config-dir DIR]");
            ExitCode::from(2)
        }
    }
}

/// How web pages are checked on the WebSocket port.
fn origin_policy(args: &Args) -> rfb_tokio::ws::OriginPolicy {
    use rfb_tokio::ws::OriginPolicy;
    if args.ws_origins.iter().any(|o| o == "*") {
        OriginPolicy::Any
    } else if args.ws_origins.is_empty() {
        OriginPolicy::SameHost
    } else {
        OriginPolicy::List(args.ws_origins.clone())
    }
}

/// The WebSocket port needs TLS unless it is on this computer: a browser
/// would send passwords over it in the clear.
fn check_websocket(args: &Args, access: &config::Access) -> Result<(), String> {
    match args.ws {
        Some(ws) if access.identity.is_none() && !ws.ip().is_loopback() => Err(format!(
            "the WebSocket port {ws} needs TLS (--tls-self-signed): browsers would send passwords in the clear."
        )),
        _ => Ok(()),
    }
}

/// What the dashboard shows about the setup.
fn live_info(args: &Args, access: &config::Access, authenticated: bool) -> LiveInfo {
    let mut fingerprints = Vec::new();
    if let Some(fp) = &access.ra2_fingerprint {
        fingerprints.push(("RSA-AES key".to_owned(), format_fingerprint(fp)));
    }
    if let Some(id) = &access.identity {
        fingerprints.push((
            "Certificate".to_owned(),
            format_fingerprint(&id.fingerprint),
        ));
    }
    let offered = vec![
        (
            "VeNCrypt over TLS".to_owned(),
            "Certificate you pin in the viewer".to_owned(),
            access.identity.is_some(),
        ),
        (
            "RSA-AES".to_owned(),
            "Encrypted, needs user accounts".to_owned(),
            access.ra2_fingerprint.is_some(),
        ),
        (
            "User accounts".to_owned(),
            "Name and password, Argon2id".to_owned(),
            access.authenticator.is_some(),
        ),
        (
            "Shared password (VNC Auth)".to_owned(),
            "Legacy, 8 characters".to_owned(),
            access.authenticator.is_none() && args.password.is_some(),
        ),
        (
            "Operator approval".to_owned(),
            "Each new client waits for you".to_owned(),
            args.require_approval,
        ),
        (
            "WebSocket (browsers)".to_owned(),
            if access.identity.is_some() {
                "wss: TLS, then user name and password".to_owned()
            } else {
                "ws on this computer only".to_owned()
            },
            args.ws.is_some(),
        ),
        (
            "H.264 video".to_owned(),
            "Opt-in, for clients that ask; OpenH264 from Cisco".to_owned(),
            args.h264,
        ),
        (
            "No authentication".to_owned(),
            "Anyone who can connect gets in".to_owned(),
            !authenticated,
        ),
    ];
    LiveInfo {
        listen: args.listen.to_string(),
        websocket: args.ws.map(|w| w.to_string()),
        access: access.description.clone(),
        fingerprints,
        offered,
    }
}

/// Everything that is running once start-up is done.
struct Running {
    server: Arc<Server>,
    listener: TcpListener,
    ws_listener: Option<TcpListener>,
    source: String,
    info: LiveInfo,
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
        log(&e);
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
        log(&e);
        ExitCode::from(2)
    })?;
    let authenticated = access.authenticator.is_some() || args.password.is_some();
    if !authenticated && !args.listen.ip().is_loopback() && !args.allow_no_auth {
        log(&format!(
            "refusing to listen on {} without credentials. Add a user \
             (`farpane-server user add NAME`), set a password with --password-file or \
             $FARPANE_PASSWORD, or pass --allow-no-auth.",
            args.listen
        ));
        return Err(ExitCode::from(2));
    }
    if access.identity.is_none() && authenticated && !args.listen.ip().is_loopback() {
        note(
            &events,
            Level::Warning,
            "warning: no TLS; the connection is not encrypted (use --tls-self-signed)",
        );
    }
    if let Some(p) = &args.password
        && access.authenticator.is_none()
        && p.len() > 8
    {
        note(
            &events,
            Level::Warning,
            "warning: VNC password authentication checks only the first 8 characters",
        );
    }
    check_websocket(args, &access).map_err(|e| {
        log(&e);
        ExitCode::from(2)
    })?;
    // Opening a screen may wait for the user (the Wayland approval dialog), so
    // keep it off the async threads.
    let (a, dir) = (args.clone(), config_dir.clone());
    let (source, input, what) = tokio::task::spawn_blocking(move || open_source(&a, &dir))
        .await
        .map_err(|e| {
            log(&e.to_string());
            ExitCode::FAILURE
        })?
        .map_err(|e| {
            log(&e);
            ExitCode::FAILURE
        })?;
    let settings = Settings {
        fps: args.fps,
        view_only: args.view_only,
    };
    let mut server =
        Server::new(source, input, access.template.clone(), settings).map_err(|e| {
            log(&format!("cannot start: {e}"));
            ExitCode::FAILURE
        })?;
    if let Some(auth) = access.authenticator.clone() {
        server.set_policy(move |user| auth.permission(user));
    }
    if let Some(max) = args.max_connections {
        server.set_max_connections(max);
    }
    server.set_require_approval(args.require_approval);
    server.set_video(video_codec(args, &config_dir).map_err(|e| {
        log(&e);
        ExitCode::from(2)
    })?);
    let sink = events.clone();
    server.set_events(move |e| {
        let level = match e {
            Event::AuthFailed { .. } | Event::HandshakeFailed { .. } | Event::Denied { .. } => {
                Level::Warning
            }
            _ => Level::Info,
        };
        note(&sink, level, &describe(e));
    });
    let server = Arc::new(server);
    let listener = TcpListener::bind(args.listen).await.map_err(|e| {
        log(&format!("cannot listen on {}: {e}", args.listen));
        ExitCode::FAILURE
    })?;
    let ws_listener = match args.ws {
        Some(ws) => {
            server.set_websocket(
                access.identity.as_ref().map(|i| Arc::clone(&i.config)),
                origin_policy(args),
            );
            let l = TcpListener::bind(ws).await.map_err(|e| {
                log(&format!("cannot listen on {ws}: {e}"));
                ExitCode::FAILURE
            })?;
            note(
                &events,
                Level::Info,
                &format!(
                    "WebSocket clients: {}://{ws}",
                    if access.identity.is_some() {
                        "wss"
                    } else {
                        "ws"
                    }
                ),
            );
            Some(l)
        }
        None => None,
    };
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
    let info = live_info(args, &access, authenticated);
    for (label, text) in &info.fingerprints {
        let what = if label == "Certificate" {
            "certificate"
        } else {
            "RSA-AES key"
        };
        note(&events, Level::Info, &format!("{what} SHA-256 {text}"));
    }
    Ok(Running {
        server,
        listener,
        ws_listener,
        source: what,
        info,
        events,
        config_dir,
    })
}

/// Accept clients on the listeners until `shutdown` completes or a listener
/// fails.
async fn serve_headless(running: Running, shutdown: impl Future<Output = ()>) -> ExitCode {
    let ws = async {
        match running.ws_listener {
            Some(l) => Arc::clone(&running.server).listen_ws(l).await,
            None => std::future::pending().await,
        }
    };
    tokio::select! {
        result = Arc::clone(&running.server).listen(running.listener) => {
            if let Err(e) = result {
                log(&e.to_string());
                return ExitCode::FAILURE;
            }
        }
        result = ws => {
            if let Err(e) = result {
                log(&format!("WebSocket listener: {e}"));
                return ExitCode::FAILURE;
            }
        }
        () = shutdown => {}
    }
    ExitCode::SUCCESS
}

#[allow(clippy::too_many_lines)] // start-up, headless and window paths
fn main() -> ExitCode {
    let mut raw = std::env::args().skip(1);
    match raw.next().as_deref() {
        Some("user") => return user_command(),
        Some("h264") => return h264_command(),
        #[cfg(windows)]
        Some("service") => return service::command(raw),
        _ => {}
    }
    let (cli, args) =
        match parse().and_then(|cli| with_saved_settings(cli.clone()).map(|a| (cli, a))) {
            Ok(pair) => pair,
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
        return rt.block_on(serve_headless(running, async {
            let _ = tokio::signal::ctrl_c().await;
        }));
    }
    // The window on this thread, the server on the runtime's.
    let Running {
        server,
        listener,
        ws_listener,
        source,
        info,
        events,
        config_dir,
    } = running;
    let listen_task = Arc::new(std::sync::Mutex::new(Listeners {
        rfb: (args.listen, rt.spawn(Arc::clone(&server).listen(listener))),
        ws: ws_listener
            .zip(args.ws)
            .map(|(l, addr)| (addr, rt.spawn(Arc::clone(&server).listen_ws(l)))),
    }));
    let applier: backend::Applier = {
        let (server, events, config_dir, handle) = (
            Arc::clone(&server),
            events.clone(),
            config_dir.clone(),
            rt.handle().clone(),
        );
        Arc::new(move |saved: &ServerSettings| {
            apply_live(
                &cli,
                saved,
                &server,
                &events,
                &config_dir,
                &handle,
                &listen_task,
            )
        })
    };
    let backend = Arc::new(LiveBackend {
        server,
        source,
        info: std::sync::Mutex::new(info),
        applier: Some(applier),
    });
    let mut dashboard = Dashboard::new(backend, events, config_dir.join("users.toml"), config_dir);
    dashboard.set_screenshot(args.ui_screenshot.clone(), args.page, args.ui_delay);
    if let Some(theme) = args.theme {
        dashboard.set_theme(theme);
    }
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

/// The accept loops of a running server and where each listens.
struct Listeners {
    rfb: (SocketAddr, tokio::task::JoinHandle<std::io::Result<()>>),
    ws: Option<(SocketAddr, tokio::task::JoinHandle<std::io::Result<()>>)>,
}

/// Put saved settings to work in the running server: how clients sign in,
/// view-only, approval, and where it listens. Nothing changes if any step
/// fails; connections already made are left alone.
fn apply_live(
    cli: &Args,
    saved: &ServerSettings,
    server: &Arc<Server>,
    events: &EventLog,
    config_dir: &std::path::Path,
    handle: &tokio::runtime::Handle,
    listen_task: &std::sync::Mutex<Listeners>,
) -> Result<LiveInfo, String> {
    let args = merge_settings(cli.clone(), saved)?;
    let tls = tls_choice(&args)?;
    let sink = events.clone();
    let access = config::choose(
        config_dir,
        args.password.clone(),
        &tls,
        args.ra2,
        move |l| {
            note(&sink, Level::Warning, &l);
        },
    )
    .map_err(|e| e.clone())?;
    let authenticated = access.authenticator.is_some() || args.password.is_some();
    if !authenticated && !args.listen.ip().is_loopback() && !args.allow_no_auth {
        return Err(format!(
            "listening on {} needs credentials: add a user first.",
            args.listen
        ));
    }
    check_websocket(&args, &access)?;
    // Bind new addresses before letting go of the old ones.
    let mut current = listen_task
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let new_rfb = if current.rfb.0 == args.listen {
        None
    } else {
        Some(
            handle
                .block_on(TcpListener::bind(args.listen))
                .map_err(|e| format!("cannot listen on {}: {e}.", args.listen))?,
        )
    };
    let current_ws = current.ws.as_ref().map(|w| w.0);
    let new_ws = match (args.ws, current_ws) {
        (Some(want), have) if have != Some(want) => Some(
            handle
                .block_on(TcpListener::bind(want))
                .map_err(|e| format!("cannot listen on {want}: {e}."))?,
        ),
        _ => None,
    };
    if let Some(listener) = new_rfb {
        current.rfb.1.abort();
        current.rfb.1 = handle.spawn(Arc::clone(server).listen(listener));
        current.rfb.0 = args.listen;
        note(
            events,
            Level::Info,
            &format!("now listening on {}", args.listen),
        );
    }
    // Certificate and origins apply to the next WebSocket connection.
    server.set_websocket(
        access.identity.as_ref().map(|i| Arc::clone(&i.config)),
        origin_policy(&args),
    );
    match (args.ws, new_ws) {
        (Some(want), Some(listener)) => {
            if let Some(old) = current.ws.take() {
                old.1.abort();
            }
            current.ws = Some((want, handle.spawn(Arc::clone(server).listen_ws(listener))));
            note(
                events,
                Level::Info,
                &format!("WebSocket clients now at {want}"),
            );
        }
        (None, _) => {
            if let Some(old) = current.ws.take() {
                old.1.abort();
                note(events, Level::Info, "the WebSocket port was closed");
            }
        }
        (Some(_), None) => {}
    }
    drop(current);
    let policy = access.authenticator.clone().map(|auth| {
        let policy: rfb_server::Policy = Arc::new(move |user| auth.permission(user));
        policy
    });
    server.replace_options(access.template.clone(), policy);
    server.set_view_only_for_all(args.view_only);
    server.set_require_approval(args.require_approval);
    server.set_video(video_codec(&args, config_dir)?);
    Ok(live_info(&args, &access, authenticated))
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

#[cfg(test)]
mod tests {
    use super::*;
    use farpane_server::settings::TlsMode;

    fn args(words: &[&str]) -> Args {
        let words: Vec<String> = words.iter().map(|w| (*w).to_owned()).collect();
        parse_from(words.into_iter()).unwrap()
    }

    #[test]
    fn saved_settings_fill_in_what_the_command_line_leaves_open() {
        let saved = ServerSettings {
            listen: Some("127.0.0.1:5999".into()),
            tls: TlsMode::SelfSigned,
            ra2: true,
            view_only: true,
            require_approval: true,
            ..ServerSettings::default()
        };
        let merged = merge_settings(args(&[]), &saved).unwrap();
        assert_eq!(merged.listen, "127.0.0.1:5999".parse().unwrap());
        assert!(merged.self_signed && merged.ra2 && merged.view_only && merged.require_approval);
        // An address on the command line wins; settings never turn off a flag it sets.
        let cli = args(&["--listen", "127.0.0.1:5901", "--ra2"]);
        let merged = merge_settings(cli, &ServerSettings::default()).unwrap();
        assert_eq!(merged.listen, "127.0.0.1:5901".parse().unwrap());
        assert!(merged.ra2);
        assert!(!merged.self_signed && !merged.view_only);
    }

    #[test]
    #[allow(clippy::too_many_lines)] // one scenario, step by step
    fn applying_settings_live_changes_the_running_server_and_moves_the_listener() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let events = EventLog::new();
        let server = Arc::new(
            Server::new(
                rfb_server::TestPattern::new(64, 64),
                rfb_server::NoInput,
                rfb_tokio::ServerOptions::new(rfb_tokio::ServerAuth::None, 64, 64, b"live"),
                Settings::default(),
            )
            .unwrap(),
        );
        let first = rt.block_on(TcpListener::bind("127.0.0.1:0")).unwrap();
        let first_addr = first.local_addr().unwrap();
        let task = std::sync::Mutex::new(Listeners {
            rfb: (first_addr, rt.spawn(Arc::clone(&server).listen(first))),
            ws: None,
        });
        let mut cli = args(&["--config-dir", dir.path().to_str().unwrap()]);
        cli.listen = first_addr;
        cli.listen_given = true;
        let apply = |saved: &ServerSettings| {
            apply_live(
                &cli,
                saved,
                &server,
                &events,
                dir.path(),
                rt.handle(),
                &task,
            )
        };
        // View-only and approval take effect at once.
        let info = apply(&ServerSettings {
            view_only: true,
            require_approval: true,
            ..ServerSettings::default()
        })
        .unwrap();
        assert!(
            info.offered
                .iter()
                .any(|o| o.0 == "Operator approval" && o.2)
        );
        // Self-signed TLS makes a certificate and shows its fingerprint.
        let info = apply(&ServerSettings {
            tls: TlsMode::SelfSigned,
            ..ServerSettings::default()
        })
        .unwrap();
        assert!(
            info.fingerprints.iter().any(|f| f.0 == "Certificate"),
            "{info:?}"
        );
        // Moving to another address: the new port answers, the old one is closed.
        let spare = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let spare_addr = spare.local_addr().unwrap();
        drop(spare);
        let moved_cli = Args {
            listen_given: false,
            ..cli.clone()
        };
        apply_live(
            &moved_cli,
            &ServerSettings {
                listen: Some(spare_addr.to_string()),
                ..ServerSettings::default()
            },
            &server,
            &events,
            dir.path(),
            rt.handle(),
            &task,
        )
        .unwrap();
        assert_eq!(task.lock().unwrap().rfb.0, spare_addr);
        rt.block_on(async {
            use tokio::io::AsyncReadExt;
            let mut banner = [0u8; 4];
            let mut client = tokio::net::TcpStream::connect(spare_addr).await.unwrap();
            client.read_exact(&mut banner).await.unwrap();
            assert_eq!(&banner, b"RFB ");
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            assert!(
                tokio::net::TcpStream::connect(first_addr).await.is_err(),
                "the old address is released"
            );
        });
        // A WebSocket port on this computer opens and closes while running.
        let ws_spare = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let ws_addr = ws_spare.local_addr().unwrap();
        drop(ws_spare);
        let ws_cli = Args {
            listen_given: false,
            ..cli.clone()
        };
        let live_ws = |saved: ServerSettings| {
            apply_live(
                &ws_cli,
                &saved,
                &server,
                &events,
                dir.path(),
                rt.handle(),
                &task,
            )
        };
        let info = live_ws(ServerSettings {
            listen: Some(spare_addr.to_string()),
            websocket: Some(ws_addr.to_string()),
            ..ServerSettings::default()
        })
        .unwrap();
        assert_eq!(
            info.websocket.as_deref(),
            Some(ws_addr.to_string().as_str())
        );
        rt.block_on(async {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let tcp = tokio::net::TcpStream::connect(ws_addr).await.unwrap();
            let mut socket = rfb_tokio::ws::connect(tcp, &ws_addr.to_string(), None)
                .await
                .unwrap();
            let mut banner = [0u8; 4];
            socket.read_exact(&mut banner).await.unwrap();
            assert_eq!(&banner, b"RFB ");
            let _ = socket.shutdown().await;
        });
        // Off again: the port closes.
        let info = live_ws(ServerSettings {
            listen: Some(spare_addr.to_string()),
            ..ServerSettings::default()
        })
        .unwrap();
        assert_eq!(info.websocket, None);
        rt.block_on(async {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            assert!(tokio::net::TcpStream::connect(ws_addr).await.is_err());
        });
        // A WebSocket port reachable from the network needs TLS.
        let refused = live_ws(ServerSettings {
            listen: Some(spare_addr.to_string()),
            websocket: Some("0.0.0.0:0".into()),
            ..ServerSettings::default()
        });
        assert!(refused.unwrap_err().contains("needs TLS"));
        // Reachable from other machines without users is refused, and nothing changes.
        let open_cli = Args {
            listen_given: false,
            ..cli.clone()
        };
        let refused = apply_live(
            &open_cli,
            &ServerSettings {
                listen: Some("0.0.0.0:0".into()),
                ..ServerSettings::default()
            },
            &server,
            &events,
            dir.path(),
            rt.handle(),
            &task,
        );
        assert!(refused.unwrap_err().contains("credentials"));
        assert_eq!(task.lock().unwrap().rfb.0, spare_addr, "nothing changed");
    }
}
