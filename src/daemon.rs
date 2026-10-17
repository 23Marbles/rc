use std::{env, ffi::OsString, fs, net::IpAddr, path::PathBuf, sync::LazyLock};

use anyhow::Context;
#[cfg(not(windows))]
use directories::BaseDirs;
use serde::{Deserialize, Serialize};
use service_manager::{
    RestartPolicy, ServiceInstallCtx, ServiceLabel, ServiceManager, ServiceStartCtx, ServiceStatus,
    ServiceStatusCtx, ServiceStopCtx, ServiceUninstallCtx,
};

use crate::daemon::users::USER_STORE_FILE_PATH;

pub mod users;
#[cfg(windows)]
pub mod windows_daemon;
pub const SERVICE_NAME: &str = "rc.server-daemon";

pub static BASE_DATA_PATH: LazyLock<PathBuf> = LazyLock::new(|| {
    #[cfg(windows)]
    let base_path = PathBuf::from(
        std::env::var_os("ProgramData").expect("ProgramData environment variable is not set"),
    )
    .join("rc-ssh");
    #[cfg(not(windows))]
    let base_path = BaseDirs::new()
        .expect("Could not determine the user configuration directory")
        .config_dir()
        .join("rc-ssh");

    if !base_path.exists() {
        std::fs::create_dir_all(&base_path).unwrap();
    }

    base_path
});

pub const DEFAULT_LOG_FILE_NAME: &str = "RC_SERVER_LOG.txt";

const CONFIG_FILE_NAME: &str = "rc-server-config.toml";

#[derive(Debug, Serialize, Deserialize)]
pub struct Config {
    pub ip: IpAddr,
    pub port: Option<u16>,
    pub log_path: Option<PathBuf>,
}

pub fn write_cfg_file(cfg: Config) -> anyhow::Result<()> {
    let file_path = BASE_DATA_PATH.clone().join(CONFIG_FILE_NAME);

    let mut buf = toml::ser::Buffer::new();

    cfg.serialize(toml::Serializer::new(&mut buf))?;

    fs::write(file_path, buf.to_string())?;

    Ok(())
}

pub fn read_cfg_file() -> anyhow::Result<Config> {
    let file_path = BASE_DATA_PATH.clone().join(CONFIG_FILE_NAME);

    let file_contents = fs::read_to_string(&file_path)
        .with_context(|| format!("Failed to read daemon config at {}", file_path.display()))?;

    let cfg = Config::deserialize(toml::Deserializer::parse(&file_contents)?)?;

    Ok(cfg)
}

pub fn run_daemon() -> anyhow::Result<()> {
    #[cfg(windows)]
    {
        windows_daemon::run()?;
    }
    #[cfg(not(windows))]
    {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()?;

        let cfg = read_cfg_file()?;
        let user_store = crate::daemon::users::UserStore::read_from_file(
            &crate::daemon::users::USER_STORE_FILE_PATH,
        )?;

        runtime.block_on(crate::run_server(
            cfg,
            user_store,
            tokio_util::sync::CancellationToken::new(),
        ))?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_base_data_path() {
        let base_data_path = &*BASE_DATA_PATH;
        assert!(base_data_path.is_dir());

        #[cfg(windows)]
        assert_eq!(
            base_data_path,
            &PathBuf::from(std::env::var_os("ProgramData").unwrap()).join("rc-ssh")
        );
    }
}

static SERVICE_LABEL: LazyLock<ServiceLabel> = LazyLock::new(|| SERVICE_NAME.parse().unwrap());

pub fn daemon_status() -> anyhow::Result<()> {
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

pub fn install_daemon(
    mut args: crate::server::Args,
    exe_path: Option<PathBuf>,
    working_directory: Option<PathBuf>,
) -> anyhow::Result<()> {
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
        ip: args.get_ip_addr()?,
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

pub fn start_daemon() -> anyhow::Result<()> {
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

pub fn stop_daemon() -> anyhow::Result<()> {
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

pub fn uninstall_daemon() -> anyhow::Result<()> {
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
