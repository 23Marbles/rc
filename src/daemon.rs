use std::{fs, path::PathBuf, sync::LazyLock};

use serde::{Deserialize, Serialize};

use crate::server;

#[cfg(windows)]
pub mod windows_daemon;

pub const SERVICE_NAME: &str = "rc.server-daemon";

static CONFIG_FOLDER_PATH: LazyLock<PathBuf> = LazyLock::new(|| {
    let home = std::env::home_dir().unwrap();

    let cfg_path = home.join(r"\rc-ssh");

    if !cfg_path.exists() {
        std::fs::create_dir_all(&cfg_path).unwrap();
    }

    cfg_path
});

const CONFIG_FILE_NAME: &str = "rc-server-config.toml";

#[derive(Debug, Serialize, Deserialize)]
pub struct Config {
    pub args: server::Args,
}

pub fn write_cfg_file(cfg: Config) -> anyhow::Result<()> {
    let file_path = CONFIG_FOLDER_PATH.clone().join(CONFIG_FILE_NAME);

    let mut buf = toml::ser::Buffer::new();

    cfg.serialize(toml::Serializer::new(&mut buf))?;

    fs::write(file_path, buf.to_string())?;

    Ok(())
}

pub fn read_cfg_file() -> anyhow::Result<Config> {
    let file_path = CONFIG_FOLDER_PATH.clone().join(CONFIG_FILE_NAME);

    assert!(file_path.is_file());

    let file_contents = fs::read_to_string(file_path)?;

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
        use crate::run_server;
        use tokio_util::sync::CancellationToken;

        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()?;

        // Load your server configuration from a file here.
        let args = read_cfg_file()?.args;

        runtime.block_on(crate::run_server(args, CancellationToken::new()))?;
    }

    Ok(())
}
