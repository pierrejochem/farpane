//! Windows service host for Farpane Server.
//!
//! The server runs as a normal console program everywhere. On Windows the
//! service control manager (SCM) starts a service through a different entry
//! point: the process must call `StartServiceCtrlDispatcher`, report its state
//! while starting, running and stopping, and react to a stop request. This
//! crate does that around a plain function and can register or remove the
//! service. It knows nothing about Farpane's protocol.
//!
//! Only Windows has anything here; on other targets the crate is empty.
//!
//! # Why this crate allows `unsafe`
//!
//! The workspace denies `unsafe`. The SCM calls the service's main function
//! through a C ABI with raw wide-string arguments, and the
//! `define_windows_service!` macro of the `windows-service` crate generates
//! that `extern "system"` shim, with the `unsafe` conversion of the argument
//! array inside it. The macro's expansion is the only `unsafe` involved; this
//! crate writes none itself.
//!
//! # What it does not do
//!
//! A service runs in session 0, which has no desktop to capture. Section 19
//! of the design reference therefore splits the server into a service and a
//! worker started in the user's session; `farpane-win::session` does that
//! from the body this crate runs.

#![cfg(windows)]

use std::{
    ffi::OsString,
    sync::{OnceLock, mpsc},
    time::Duration,
};

use windows_service::{
    define_windows_service,
    service::{
        ServiceAccess, ServiceControl, ServiceControlAccept, ServiceErrorControl, ServiceExitCode,
        ServiceInfo, ServiceStartType, ServiceState, ServiceStatus, ServiceType,
    },
    service_control_handler::{self, ServiceControlHandlerResult},
    service_dispatcher,
    service_manager::{ServiceManager, ServiceManagerAccess},
};

pub use windows_service::Error;

/// How the service is named to the SCM.
pub const SERVICE_NAME: &str = "FarpaneServer";

const DISPLAY_NAME: &str = "Farpane Server";
const DESCRIPTION: &str = "Farpane Server: VNC-compatible remote desktop server";
/// How long the SCM waits for a start or stop before it assumes a hang.
const WAIT_HINT: Duration = Duration::from_secs(10);

/// Told to the service body when the SCM asks it to stop (also on shutdown).
#[derive(Debug)]
pub struct Stop(mpsc::Receiver<()>);

impl Stop {
    /// Block until a stop is requested. Returns at once if the SCM is gone.
    pub fn wait(&self) {
        let _ = self.0.recv();
    }
}

/// The program that runs as the service: it gets the stop signal and returns
/// the exit code to report (0 is success).
pub type Body = fn(Stop) -> u32;

static BODY: OnceLock<Body> = OnceLock::new();

define_windows_service!(ffi_service_main, service_main);

fn service_main(_scm_arguments: Vec<OsString>) {
    // The SCM's own arguments only repeat the name; the real ones are in the
    // registered command line, which the program parsed before calling `run`.
    // The SCM has nobody to report a failure to, so there is nothing to do
    // with one here.
    if let Some(body) = BODY.get() {
        let _ = host(*body);
    }
}

fn host(body: Body) -> Result<(), Error> {
    let (tx, rx) = mpsc::channel();
    let handle = service_control_handler::register(SERVICE_NAME, move |control| match control {
        ServiceControl::Stop | ServiceControl::Shutdown => {
            let _ = tx.send(());
            ServiceControlHandlerResult::NoError
        }
        ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
        _ => ServiceControlHandlerResult::NotImplemented,
    })?;
    let status = |state, accept, code, wait| ServiceStatus {
        service_type: ServiceType::OWN_PROCESS,
        current_state: state,
        controls_accepted: accept,
        exit_code: ServiceExitCode::Win32(code),
        checkpoint: 0,
        wait_hint: wait,
        process_id: None,
    };
    handle.set_service_status(status(
        ServiceState::StartPending,
        ServiceControlAccept::empty(),
        0,
        WAIT_HINT,
    ))?;
    handle.set_service_status(status(
        ServiceState::Running,
        ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN,
        0,
        Duration::default(),
    ))?;
    let code = body(Stop(rx));
    handle.set_service_status(status(
        ServiceState::Stopped,
        ServiceControlAccept::empty(),
        code,
        Duration::default(),
    ))
}

/// Hand this process to the SCM and run `body` as the service until it is
/// told to stop. Blocks. Fails at once (error 1063) when started from a
/// console instead of by the SCM.
///
/// # Errors
/// The dispatcher could not be started.
pub fn run(body: Body) -> Result<(), Error> {
    let _ = BODY.set(body);
    service_dispatcher::start(SERVICE_NAME, ffi_service_main)
}

/// Register the service with the SCM. `executable` is started with
/// `arguments`; it starts at boot when `auto_start` is set.
///
/// # Errors
/// No administrator rights, or the service exists already.
pub fn install(
    executable: std::path::PathBuf,
    arguments: Vec<OsString>,
    auto_start: bool,
) -> Result<(), Error> {
    let manager = ServiceManager::local_computer(
        None::<&str>,
        ServiceManagerAccess::CONNECT | ServiceManagerAccess::CREATE_SERVICE,
    )?;
    let info = ServiceInfo {
        name: OsString::from(SERVICE_NAME),
        display_name: OsString::from(DISPLAY_NAME),
        service_type: ServiceType::OWN_PROCESS,
        start_type: if auto_start {
            ServiceStartType::AutoStart
        } else {
            ServiceStartType::OnDemand
        },
        error_control: ServiceErrorControl::Normal,
        executable_path: executable,
        launch_arguments: arguments,
        dependencies: vec![],
        account_name: None, // LocalSystem
        account_password: None,
    };
    let service = manager.create_service(&info, ServiceAccess::CHANGE_CONFIG)?;
    service.set_description(DESCRIPTION)
}

/// Stop the service if it runs and remove it from the SCM.
///
/// # Errors
/// No administrator rights, or there is no such service.
pub fn uninstall() -> Result<(), Error> {
    let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)?;
    let service = manager.open_service(
        SERVICE_NAME,
        ServiceAccess::QUERY_STATUS | ServiceAccess::STOP | ServiceAccess::DELETE,
    )?;
    if service.query_status()?.current_state != ServiceState::Stopped {
        service.stop()?;
    }
    service.delete()
}
