use std::{
    env,
    ffi::OsString,
    net::SocketAddr,
    path::PathBuf,
    sync::{Arc, LazyLock},
};

use anyhow::Result;
use clap::Parser;
use rc::server::{self, Server};
use russh::{
    keys::{Algorithm, PrivateKey},
    server::Server as _,
};
use service_manager::{
    RestartPolicy, ServiceInstallCtx, ServiceLabel, ServiceManager, ServiceStartCtx, ServiceStatus,
    ServiceStatusCtx, ServiceStopCtx, ServiceUninstallCtx,
};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

#[derive(Debug, clap::Parser)]
pub enum ServerRunnerArgs {
    #[command(visible_alias("uninstall"))]
    UninstallDaemon,
    #[command(visible_alias("stop"))]
    StopDaemon,
    /// Start the server daemon
    #[command(visible_alias("start"))]
    StartDaemon {
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
    /// Run the server as a daemon
    RunDaemon {
        #[command(flatten)]
        args: server::Args,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = ServerRunnerArgs::parse();

    match args {
        ServerRunnerArgs::StartDaemon {
            args,
            exe_path,
            working_directory,
        } => start_daemon(args, exe_path, working_directory),
        ServerRunnerArgs::Run { args } => run_server(args, CancellationToken::new()).await,
        ServerRunnerArgs::UninstallDaemon => uninstall_daemon(),
        ServerRunnerArgs::StopDaemon => stop_daemon(),
        ServerRunnerArgs::RunDaemon { args } => todo!(),
    }
}

static SERVICE_LABEL: LazyLock<ServiceLabel> =
    LazyLock::new(|| "rc.server-daemon".parse().unwrap());

fn uninstall_daemon() -> Result<()> {
    // Get generic service by detecting what is available on the platform
    let manager = <dyn ServiceManager>::native().expect("Failed to detect management platform");

    match manager.status(ServiceStatusCtx {
        label: SERVICE_LABEL.clone(),
    })? {
        ServiceStatus::NotInstalled => println!("Service not installed"),
        ServiceStatus::Running => {
            println!("Service running\nStopping...");
            manager
                .stop(ServiceStopCtx {
                    label: SERVICE_LABEL.clone(),
                })
                .expect("Failed to stop service");

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

fn stop_daemon() -> Result<()> {
    // Get generic service by detecting what is available on the platform
    let manager = <dyn ServiceManager>::native().expect("Failed to detect management platform");

    match manager.status(ServiceStatusCtx {
        label: SERVICE_LABEL.clone(),
    })? {
        ServiceStatus::NotInstalled => println!("Service not installed"),
        ServiceStatus::Running => {
            println!("Service running\nStopping...");
            manager
                .stop(ServiceStopCtx {
                    label: SERVICE_LABEL.clone(),
                })
                .expect("Failed to stop service")
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

fn start_daemon(
    args: server::Args,
    exe_path: Option<PathBuf>,
    working_directory: Option<PathBuf>,
) -> Result<()> {
    // Get generic service by detecting what is available on the platform
    let manager = <dyn ServiceManager>::native().expect("Failed to detect management platform");

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

            let server::Args {
                ip_addr,
                users,
                socket,
            } = args;

            if let Some(port) = socket {
                args_vec.push(OsString::from("--socket"));
                args_vec.push(OsString::from(port.to_string()));
            };

            args_vec.push(OsString::from(ip_addr.to_string()));

            match users {
                server::UsersArg::Manual { name, password } => {
                    args_vec.push(OsString::from("manual"));
                    args_vec.push(OsString::from(name));
                    args_vec.push(OsString::from(password));
                }
                server::UsersArg::File { path } => {
                    args_vec.push(OsString::from("file"));
                    args_vec.push(OsString::from(path));
                }
            }

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

            println!("Starting service...");

            manager
                .start(ServiceStartCtx {
                    label: SERVICE_LABEL.clone(),
                })
                .expect("Failed to start");

            println!("Started service...");
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

async fn run_server(args: server::Args, cancel: CancellationToken) -> Result<()> {
    env_logger::Builder::new()
        .filter_level(log::LevelFilter::Info)
        .init();

    let listening_addr = SocketAddr::new(args.ip_addr, args.socket.unwrap_or(2222));

    let config = server::Config {
        keys: vec![PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519)?],
        ..Default::default()
    };

    let config = Arc::new(config);
    let listener = TcpListener::bind(listening_addr).await?;

    log::info!("Listening on {listening_addr}");

    let mut server = Server;

    let _ = tokio::select! {
        result = server.run_on_socket(config, &listener) => result,
        _ = cancel.cancelled() => Ok(()),
    }?;

    Ok(())
}
