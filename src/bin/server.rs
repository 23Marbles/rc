use std::sync::Arc;

use anyhow::Result;
use rc::server::Server;
use russh::{
    keys::{Algorithm, PrivateKey},
    server::{self, Server as _},
};
use tokio::net::TcpListener;

#[tokio::main]
async fn main() -> Result<()> {
    env_logger::Builder::new()
        .filter_level(log::LevelFilter::Info)
        .init();

    let config = server::Config {
        keys: vec![PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519)?],
        ..Default::default()
    };

    let config = Arc::new(config);
    let listener = TcpListener::bind("127.0.0.1:2222").await?;

    log::info!("Listening on 127.0.0.1:2222");

    let mut server = Server;
    server.run_on_socket(config, &listener).await?;

    Ok(())
}
