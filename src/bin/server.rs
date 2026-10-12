use std::{
    net::{IpAddr, SocketAddr},
    sync::Arc,
};

use anyhow::Result;
use clap::Parser;
use rc::{Args, server::Server};
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

    let args = Args::parse();

    let listening_addr = SocketAddr::new(args.ip_addr, args.socket.unwrap_or(2222));

    let config = server::Config {
        keys: vec![PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519)?],
        ..Default::default()
    };

    let config = Arc::new(config);
    let listener = TcpListener::bind(listening_addr).await?;

    log::info!("Listening on {listening_addr}");

    let mut server = Server;
    server.run_on_socket(config, &listener).await?;

    Ok(())
}
