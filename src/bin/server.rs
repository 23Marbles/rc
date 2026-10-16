use std::{env, ffi::OsString, fs, path::PathBuf, sync::LazyLock};

use anyhow::Result;
use clap::Parser;
use rc::{
    daemon::{
        self, BASE_DATA_PATH, Config, DEFAULT_LOG_FILE_NAME, users::USER_STORE_FILE_PATH,
        write_cfg_file,
    },
    server::{self},
};
use service_manager::{
    RestartPolicy, ServiceInstallCtx, ServiceLabel, ServiceManager, ServiceStartCtx, ServiceStatus,
    ServiceStatusCtx, ServiceStopCtx, ServiceUninstallCtx,
};
use tokio_util::sync::CancellationToken;

#[derive(Debug, clap::Parser)]
pub enum ServerRunnerArgs {
    #[command(visible_alias("status"))]
    DaemonStatus,
    #[command(visible_alias("uninstall"))]
    UninstallDaemon,
    #[command(visible_alias("stop"))]
    StopDaemon,
    #[command(visible_alias("start"))]
    StartDaemon,
    /// Start the server daemon
    #[command(visible_alias("install"))]
    InstallDaemon {
        /// An optional path to the daemon exe (expected to be the same program as this).
        /// This is useful to specify if the path_to_exe cannot be determined
        #[arg(long, short = 'p')]
        exe_path: Option<PathBuf>,

        #[arg(long, short = 'd')]
        working_directory: Option<PathBuf>,

        #[command(flatten)]
        args: server::Args,
    },
    /// Run the server
    Run {
        #[command(flatten)]
        args: server::Args,
    },
    /// Run the server as a daemon, reading args from the config file
    RunDaemon,
}

fn main() -> Result<()> {
    let args = ServerRunnerArgs::parse();

    match args {
        ServerRunnerArgs::InstallDaemon {
            args,
            exe_path,
            working_directory,
        } => install_daemon(args, exe_path, working_directory),
        ServerRunnerArgs::Run { args } => tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()?
            .block_on(async {
                let shutdown = CancellationToken::new();
                let shutdown_cloned = shutdown.clone();

                let cfg = Config {
                    ip: args.ip_addr,
                    port: args.socket,
                    log_path: args.log_path,
                };

                let user_store = args.users.to_user_store()?;

                tokio::select! {
                    res = rc::run_server(cfg, user_store, shutdown_cloned) => {
                        res?;
                    }

                    _ = tokio::signal::ctrl_c() => {
                        shutdown.cancel();
                    }
                }

                Ok(())
            }),
        ServerRunnerArgs::UninstallDaemon => uninstall_daemon(),
        ServerRunnerArgs::StartDaemon => start_daemon(),
        ServerRunnerArgs::StopDaemon => stop_daemon(),
        ServerRunnerArgs::RunDaemon => rc::daemon::run_daemon(),
        ServerRunnerArgs::DaemonStatus => daemon_status(),
    }
}

static SERVICE_LABEL: LazyLock<ServiceLabel> =
    LazyLock::new(|| daemon::SERVICE_NAME.parse().unwrap());

fn daemon_status() -> Result<()> {
    // Get generic service by detecting what is available on the platform
    let manager = <dyn ServiceManager>::native().expect("Failed to detect management platform");

    match manager.status(ServiceStatusCtx {
        label: SERVICE_LABEL.clone(),
    })? {
        ServiceStatus::NotInstalled => {
            println!("Service not installed");
        }
        ServiceStatus::Running => println!("Service running"),
        ServiceStatus::Stopped(reason) => {
            println!(
                "Service stopped{}",
                reason.map_or(String::new(), |reason| format!(" (reason: {reason})"))
            );
        }
    };

    Ok(())
}

fn install_daemon(
    mut args: server::Args,
    exe_path: Option<PathBuf>,
    working_directory: Option<PathBuf>,
) -> Result<()> {
    // Get generic service by detecting what is available on the platform
    let manager = <dyn ServiceManager>::native().expect("Failed to detect management platform");

    if let Some(path) = args.log_path.clone() {
        if !path.exists() {
            fs::OpenOptions::new()
                .create_new(true)
                .append(true)
                .open(path)?;
        }
    }

    args.canonicalize()?;

    write_cfg_file(Config {
        ip: args.ip_addr,
        port: args.socket,
        log_path: Some(
            args.log_path
                .unwrap_or(BASE_DATA_PATH.join(DEFAULT_LOG_FILE_NAME)),
        ),
    })?;

    let store = args.users.to_user_store()?;

    store
        .write_to_file(&USER_STORE_FILE_PATH)
        .expect("`UserStore` save failed");

    match manager.status(ServiceStatusCtx {
        label: SERVICE_LABEL.clone(),
    })? {
        ServiceStatus::NotInstalled => {
            let path = exe_path
                .ok_or(())
                .or_else(|_| env::current_exe())?
                .canonicalize()?;

            let mut args_vec = Vec::new();

            args_vec.push(OsString::from("run-daemon"));

            println!("Installing service");
            manager
                .install(ServiceInstallCtx {
                    label: SERVICE_LABEL.clone(),
                    program: path,
                    args: args_vec,
                    contents: None,
                    username: None,
                    working_directory: working_directory,
                    environment: None,
                    autostart: true,
                    restart_policy: RestartPolicy::default(),
                })
                .expect("Failed to install");
        }
        ServiceStatus::Running => println!("Service already installed and running"),
        ServiceStatus::Stopped(reason) => {
            println!(
                "Service stopped{}\nRestarting service",
                reason.map_or(String::new(), |reason| format!(" (reason: {reason})"))
            );

            manager
                .start(ServiceStartCtx {
                    label: SERVICE_LABEL.clone(),
                })
                .expect("Failed to start")
        }
    };

    Ok(())
}

fn start_daemon() -> Result<()> {
    // Get generic service by detecting what is available on the platform
    let manager = <dyn ServiceManager>::native().expect("Failed to detect management platform");

    match manager.status(ServiceStatusCtx {
        label: SERVICE_LABEL.clone(),
    })? {
        ServiceStatus::NotInstalled => {
            println!("Service not installed");
        }
        ServiceStatus::Running => println!("Service already running"),
        ServiceStatus::Stopped(reason) => {
            println!(
                "Service stopped{}\nRestarting service",
                reason.map_or(String::new(), |reason| format!(" (reason: {reason})"))
            );

            manager
                .start(ServiceStartCtx {
                    label: SERVICE_LABEL.clone(),
                })
                .expect("Failed to start")
        }
    };

    Ok(())
}

fn stop_daemon() -> Result<()> {
    // Get generic service by detecting what is available on the platform
    let manager = <dyn ServiceManager>::native().expect("Failed to detect management platform");

    match manager.status(ServiceStatusCtx {
        label: SERVICE_LABEL.clone(),
    })? {
        ServiceStatus::NotInstalled => println!("Service not installed"),
        ServiceStatus::Running => {
            println!("Service running\nStopping...");
            manager.stop(ServiceStopCtx {
                label: SERVICE_LABEL.clone(),
            })?;
        }
        ServiceStatus::Stopped(reason) => {
            println!(
                "Service already stopped{}",
                reason.map_or(String::new(), |reason| format!(" (reason: {reason})"))
            )
        }
    };

    Ok(())
}

fn uninstall_daemon() -> Result<()> {
    // Get generic service by detecting what is available on the platform
    let manager = <dyn ServiceManager>::native().expect("Failed to detect management platform");

    match manager.status(ServiceStatusCtx {
        label: SERVICE_LABEL.clone(),
    })? {
        ServiceStatus::NotInstalled => println!("Service not installed"),
        ServiceStatus::Running => {
            println!("Service running\nStopping...");
            manager.stop(ServiceStopCtx {
                label: SERVICE_LABEL.clone(),
            })?;

            println!("Uninstalling service");
            manager
                .uninstall(ServiceUninstallCtx {
                    label: SERVICE_LABEL.clone(),
                })
                .expect("Failed to uninstall service");
        }
        ServiceStatus::Stopped(reason) => {
            println!(
                "Service stopped{}",
                reason.map_or(String::new(), |reason| format!(" (reason: {reason})"))
            );

            println!("Uninstalling service");
            manager
                .uninstall(ServiceUninstallCtx {
                    label: SERVICE_LABEL.clone(),
                })
                .expect("Failed to uninstall service");
        }
    };

    Ok(())
}
