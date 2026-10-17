use std::{fs, net::IpAddr, path::PathBuf, sync::LazyLock};

use anyhow::Context;
#[cfg(not(windows))]
use directories::BaseDirs;
use serde::{Deserialize, Serialize};

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
