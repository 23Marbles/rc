use anyhow::Result;
use clap::Parser;
use rc::{daemon::Config, server};
use std::path::PathBuf;
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
        } => rc::daemon::install_daemon(args, exe_path, working_directory),
        ServerRunnerArgs::Run { args } => {
            let cfg = Config {
                ip: args.get_ip_addr()?,
                port: args.socket,
                log_path: args.log_path,
            };

            let user_store = args.users.to_user_store()?;

            tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()?
                .block_on(async {
                    let shutdown = CancellationToken::new();
                    let shutdown_cloned = shutdown.clone();

                    tokio::select! {
                        res = rc::run_server(cfg, user_store, shutdown_cloned) => {
                            res?;
                        }

                        _ = tokio::signal::ctrl_c() => {
                            shutdown.cancel();
                        }
                    }

                    Ok(())
                })
        }
        ServerRunnerArgs::UninstallDaemon => rc::daemon::uninstall_daemon(),
        ServerRunnerArgs::StartDaemon => rc::daemon::start_daemon(),
        ServerRunnerArgs::StopDaemon => rc::daemon::stop_daemon(),
        ServerRunnerArgs::RunDaemon => rc::daemon::run_daemon(),
        ServerRunnerArgs::DaemonStatus => rc::daemon::daemon_status(),
    }
}
