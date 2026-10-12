use std::net::IpAddr;

#[derive(Debug, clap::Parser)]
pub struct Args {
    pub ip_addr: IpAddr,
    #[arg(long, short)]
    pub socket: Option<u16>,
}

pub mod server;
