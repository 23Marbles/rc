use std::{ffi::OsString, fs::OpenOptions, io::Write, time::Duration};

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

use crate::daemon::{
    BASE_DATA_PATH, DEFAULT_LOG_FILE_NAME, SERVICE_NAME, read_cfg_file,
    users::{USER_STORE_FILE_PATH, UserStore},
};

define_windows_service!(ffi_service_main, service_main);

pub fn run() -> windows_service::Result<()> {
    service_dispatcher::start(SERVICE_NAME, ffi_service_main)
}

fn service_main(_args: Vec<OsString>) {
    if let Err(error) = run_service() {
        report_service_error(&error);
    }
}

fn report_service_error(error: &anyhow::Error) {
    if log::log_enabled!(log::Level::Error) {
        log::error!("Service failed: {error:#}");
        return;
    }

    let log_path = BASE_DATA_PATH.join(DEFAULT_LOG_FILE_NAME);
    let result = (|| -> std::io::Result<()> {
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)?;
        writeln!(file, "Service failed: {error:#}")
    })();

    if let Err(log_error) = result {
        eprintln!(
            "Service failed: {error:#}; could not write startup error to {}: {log_error}",
            log_path.display()
        );
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

    let status = |state, controls, exit_code, checkpoint, wait_hint| ServiceStatus {
        service_type: ServiceType::OWN_PROCESS,
        current_state: state,
        controls_accepted: controls,
        exit_code,
        checkpoint,
        wait_hint,
        process_id: None,
    };

    status_handle.set_service_status(status(
        ServiceState::StartPending,
        ServiceControlAccept::empty(),
        ServiceExitCode::NO_ERROR,
        1,
        Duration::from_secs(30),
    ))?;

    let result = (|| -> anyhow::Result<()> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()?;

        let cfg = read_cfg_file()?;
        let user_store = UserStore::read_from_file(&USER_STORE_FILE_PATH)?;
        let status_handle = &status_handle;

        let server = crate::run_server_with_ready(cfg, user_store, shutdown.clone(), || {
            status_handle.set_service_status(status(
                ServiceState::Running,
                ServiceControlAccept::STOP,
                ServiceExitCode::NO_ERROR,
                0,
                Duration::ZERO,
            ))?;
            Ok(())
        });

        runtime.block_on(async {
            tokio::pin!(server);

            tokio::select! {
                biased;
                _ = shutdown.cancelled() => {
                    status_handle.set_service_status(status(
                        ServiceState::StopPending,
                        ServiceControlAccept::empty(),
                        ServiceExitCode::NO_ERROR,
                        1,
                        Duration::from_secs(30),
                    ))?;
                    server.as_mut().await
                }
                result = server.as_mut() => result,
            }
        })
    })();

    let exit_code = if result.is_ok() {
        ServiceExitCode::NO_ERROR
    } else {
        ServiceExitCode::ServiceSpecific(1)
    };
    status_handle.set_service_status(status(
        ServiceState::Stopped,
        ServiceControlAccept::empty(),
        exit_code,
        0,
        Duration::ZERO,
    ))?;

    result
}
