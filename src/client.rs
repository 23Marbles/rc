pub use russh::client::*;

use std::net::IpAddr;

#[derive(Debug, clap::Parser)]
pub struct Args {
    pub ip_addr: IpAddr,
    #[arg(long, short, alias = "name")]
    pub user: String,
    #[arg(long, short)]
    pub password: String,
    #[arg(long, short)]
    pub socket: Option<u16>,
}
