//! `farpane-server service install | uninstall | run`: the Windows service.
//!
//! `install` registers the service with the service control manager and
//! locks the system config folder down to administrators and SYSTEM; `run`
//! is the command line the manager starts. A service has no console, so
//! everything `log` prints also goes to `service.log` in the config folder.

use std::{
    ffi::OsString,
    fs::{File, OpenOptions},
    io::Write,
    process::{Command, ExitCode},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
};

use farpane_win::remote::{RemoteInput, RemoteSource};

use farpane_winsvc::Stop;

use super::{Args, config, parse_from, serve_headless, start, with_saved_settings};

const USAGE: &str = "\
Usage: farpane-server service install [--auto] [SERVER OPTIONS]...
       farpane-server service uninstall
       farpane-server service run [SERVER OPTIONS]...   (started by Windows, not by hand)
       farpane-server service worker [--log FILE]       (started by the service, not by hand)

  install     register the \"Farpane Server\" service (run as administrator). It
              starts when asked (`sc start FarpaneServer`), or at boot with
              --auto. The server options are stored with it; give a password
              as --password-file, never on the command line.
  uninstall   stop and remove the service.

Files live in %ProgramData%\\Farpane (users.toml, tls\\, server.toml and
service.log); only administrators and SYSTEM may read it.
";

static ARGS: OnceLock<Args> = OnceLock::new();
static LOG_FILE: OnceLock<Mutex<File>> = OnceLock::new();
/// The screen and input of the session worker, until `open_source` takes them.
static REMOTE: Mutex<Option<(RemoteSource, RemoteInput)>> = Mutex::new(None);

/// The service's screen and input (the session worker's), if this process is the service.
pub fn take_remote() -> Option<(RemoteSource, RemoteInput)> {
    REMOTE.lock().ok()?.take()
}

/// Add a line to `service.log`, if this process is the service.
pub fn write_log(line: &str) {
    if let Some(Ok(mut file)) = LOG_FILE.get().map(Mutex::lock) {
        let _ = writeln!(file, "{line}");
    }
}

pub fn command(mut words: impl Iterator<Item = String> + 'static) -> ExitCode {
    match words.next().as_deref() {
        Some("install") => report(install(words.collect())),
        Some("uninstall") => report(farpane_winsvc::uninstall().map_err(|e| e.to_string())),
        Some("run") => run(words.collect()),
        Some("worker") => worker(words.collect()),
        _ => {
            eprintln!("{USAGE}");
            ExitCode::from(2)
        }
    }
}

fn report(result: Result<(), String>) -> ExitCode {
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("farpane-server: {e}");
            ExitCode::FAILURE
        }
    }
}

fn install(mut options: Vec<String>) -> Result<(), String> {
    let auto = options
        .iter()
        .position(|w| w == "--auto")
        .map(|i| options.remove(i))
        .is_some();
    // Fail now, not at the first start, on options that cannot work.
    let words = options.clone();
    parse_from(words.into_iter()).map_err(|e| format!("{e}\n\n{USAGE}"))?;
    let dir = config::system_config_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    restrict(&dir)?;
    let exe = std::env::current_exe().map_err(|e| format!("cannot find this program: {e}"))?;
    let mut arguments: Vec<OsString> = vec!["service".into(), "run".into()];
    arguments.extend(options.into_iter().map(OsString::from));
    farpane_winsvc::install(exe, arguments, auto).map_err(|e| {
        format!("cannot register the service (run as administrator; it may exist already): {e}")
    })?;
    println!(
        "Service \"Farpane Server\" registered. Start it with: sc start {}",
        farpane_winsvc::SERVICE_NAME
    );
    Ok(())
}

/// The config folder holds password hashes and private keys: leave access to
/// administrators and SYSTEM only, for the folder and what is created in it.
fn restrict(dir: &std::path::Path) -> Result<(), String> {
    let status = Command::new("icacls")
        .arg(dir)
        .args([
            "/inheritance:r",
            "/grant:r",
            "*S-1-5-18:(OI)(CI)F",
            "*S-1-5-32-544:(OI)(CI)F",
        ])
        .output()
        .map_err(|e| format!("cannot run icacls to protect {}: {e}", dir.display()))?;
    if status.status.success() {
        Ok(())
    } else {
        Err(format!(
            "icacls could not restrict {}: {}",
            dir.display(),
            String::from_utf8_lossy(&status.stdout).trim()
        ))
    }
}

fn run(options: Vec<String>) -> ExitCode {
    let args = parse_from(options.into_iter())
        .and_then(with_saved_settings)
        .map(|mut a| {
            a.headless = true;
            a
        });
    let args = match args {
        Ok(a) => a,
        Err(e) => {
            eprintln!("farpane-server: {e}");
            return ExitCode::from(2);
        }
    };
    let dir = args
        .config_dir
        .clone()
        .unwrap_or_else(config::default_config_dir);
    let _ = std::fs::create_dir_all(&dir);
    if let Ok(file) = OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("service.log"))
    {
        let _ = LOG_FILE.set(Mutex::new(file));
    }
    let _ = ARGS.set(args);
    match farpane_winsvc::run(body) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            // Error 1063 means a console started this, not Windows.
            eprintln!(
                "farpane-server: cannot run as a service ({e}); it is started by Windows, \
                 see `farpane-server service`"
            );
            ExitCode::FAILURE
        }
    }
}

/// What the service does between start and stop.
fn body(stop: Stop) -> u32 {
    let Some(args) = ARGS.get() else { return 1 };
    let rt = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            super::log(&format!("cannot start the runtime: {e}"));
            return 1;
        }
    };
    // The screen lives in the user's session: a worker started there feeds it.
    let (hub, source, input) = farpane_win::remote::Hub::new();
    if let Ok(mut slot) = REMOTE.lock() {
        *slot = Some((source, input));
    }
    let stopping = Arc::new(AtomicBool::new(false));
    let supervisor = std::env::current_exe().ok().and_then(|exe| {
        let mut arguments: Vec<OsString> = vec!["service".into(), "worker".into()];
        arguments.push("--log".into());
        arguments.push(
            args.config_dir
                .clone()
                .unwrap_or_else(config::default_config_dir)
                .join("service.log")
                .into_os_string(),
        );
        let stopping = Arc::clone(&stopping);
        std::thread::Builder::new()
            .name("farpane-supervisor".into())
            .spawn(move || {
                farpane_win::session::supervise(&hub, &exe, &arguments, &stopping, &|l| {
                    super::log(l);
                });
            })
            .ok()
    });
    if supervisor.is_none() {
        super::log("cannot start the session worker supervisor; the service has no screen");
    }
    let Ok(running) = rt.block_on(start(args)) else {
        stopping.store(true, Ordering::Relaxed);
        super::log("the server did not start; see the lines above in service.log");
        return 1;
    };
    super::log("service started");
    let code = rt.block_on(serve_headless(running, async {
        let _ = tokio::task::spawn_blocking(move || stop.wait()).await;
    }));
    stopping.store(true, Ordering::Relaxed);
    if let Some(thread) = supervisor {
        let _ = thread.join();
    }
    super::log("service stopped");
    u32::from(code != ExitCode::SUCCESS)
}

/// `service worker`: the process the service starts in the user's session. It
/// reads input commands on standard input and writes screen frames to
/// standard output (anonymous pipes made by the service); it ends when the
/// service closes its end.
fn worker(options: Vec<String>) -> ExitCode {
    let mut log = None;
    let mut words = options.into_iter();
    while let Some(word) = words.next() {
        if word == "--log" {
            log = words.next();
        }
    }
    let note = |line: &str| {
        if let Some(path) = &log
            && let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path)
        {
            let _ = writeln!(file, "[worker] {line}");
        }
    };
    let (source, input) = match farpane_win::open() {
        Ok(pair) => pair,
        Err(e) => {
            note(&format!("cannot open the screen: {e}"));
            return ExitCode::FAILURE;
        }
    };
    match farpane_win::worker::run(source, input, std::io::stdin(), std::io::stdout(), 30) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            note(&format!("worker stopped: {e}"));
            ExitCode::FAILURE
        }
    }
}
