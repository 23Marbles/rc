use std::{ffi::OsString, time::Duration};

use tokio_util::sync::CancellationToken;

use windows_service::{
    define_windows_service,
    service::{
        ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceState, ServiceStatus,
        ServiceType,
    },
    service_control_handler::{self, ServiceControlHandlerResult},
    service_dispatcher,
};

use crate::daemon::{SERVICE_NAME, read_cfg_file};

define_windows_service!(ffi_service_main, service_main);

pub fn run() -> windows_service::Result<()> {
    service_dispatcher::start(SERVICE_NAME, ffi_service_main)
}

fn service_main(_args: Vec<OsString>) {
    if let Err(error) = run_service() {
        // Write to a file or the Windows Event Log.
        eprintln!("Service failed: {error}");
    }
}

fn run_service() -> anyhow::Result<()> {
    let shutdown = CancellationToken::new();
    let stop_token = shutdown.clone();

    let status_handle =
        service_control_handler::register(SERVICE_NAME, move |event| match event {
            ServiceControl::Stop => {
                stop_token.cancel();
                ServiceControlHandlerResult::NoError
            }
            ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
            _ => ServiceControlHandlerResult::NotImplemented,
        })?;

    let status = |state, controls| ServiceStatus {
        service_type: ServiceType::OWN_PROCESS,
        current_state: state,
        controls_accepted: controls,
        exit_code: ServiceExitCode::Win32(0),
        checkpoint: 0,
        wait_hint: Duration::ZERO,
        process_id: None,
    };

    status_handle.set_service_status(status(ServiceState::Running, ServiceControlAccept::STOP))?;

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    // Load your server configuration from a file here.
    let args = read_cfg_file()?.args;

    let result = runtime.block_on(crate::run_server(args, shutdown));

    status_handle
        .set_service_status(status(ServiceState::Stopped, ServiceControlAccept::empty()))?;

    result
}
