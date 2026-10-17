use std::{
    ffi::OsStr,
    fs::OpenOptions,
    net::{IpAddr, SocketAddr},
    process::Command,
    sync::Arc,
};

use anyhow::Result;
use russh::{
    keys::{Algorithm, PrivateKey},
    server::Server,
};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

use crate::daemon::users::UserStore;

pub mod client;
pub mod daemon;
pub mod server;

fn get_tailscale_ip(machine_name: Option<impl AsRef<OsStr>>) -> anyhow::Result<IpAddr> {
    let mut program = Command::new("tailscale");
    program.args(["ip", "-4"]);

    if let Some(name) = machine_name {
        program.arg(name);
    }

    let output = program.output()?;

    let string = String::from_utf8(output.stdout)?;

    Ok(string.trim().parse()?)
}

pub async fn run_server(
    cfg: daemon::Config,
    user_store: UserStore,
    shutdown: CancellationToken,
) -> Result<()> {
    run_server_with_ready(cfg, user_store, shutdown, || Ok(())).await
}

pub async fn run_server_with_ready(
    cfg: daemon::Config,
    user_store: UserStore,
    shutdown: CancellationToken,
    on_ready: impl FnOnce() -> Result<()>,
) -> Result<()> {
    if let Some(path) = cfg.log_path {
        let file = OpenOptions::new().create(true).append(true).open(path)?;

        env_logger::Builder::new()
            .target(env_logger::Target::Pipe(Box::new(file)))
            .filter_level(log::LevelFilter::Info)
            .init();
    } else {
        env_logger::Builder::new()
            .filter_level(log::LevelFilter::Info)
            .init();
    }

    log::info!("Begin server initialisation");

    let listening_addr = SocketAddr::new(cfg.ip, cfg.port.unwrap_or(2222));

    let config = server::Config {
        keys: vec![PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519)?],
        ..Default::default()
    };

    let config = Arc::new(config);
    let listener = TcpListener::bind(listening_addr).await?;

    log::info!("Listening on {listening_addr}");
    on_ready()?;

    let mut server = server::Server {
        shutdown: shutdown.clone(),
        user_store,
    };

    let _ = tokio::select! {
        result = server.run_on_socket(config, &listener) => result,
        _ = shutdown.cancelled() => Ok(()),
    }?;

    Ok(())
}
