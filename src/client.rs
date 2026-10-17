pub use russh::client::*;

use std::{
    convert::Infallible,
    net::{IpAddr, Ipv4Addr},
    str::FromStr,
};

use crate::get_tailscale_ip;

#[derive(Debug, clap::Parser)]
pub struct Args {
    ip_addr: SendAddress,
    #[arg(long, short, alias = "name")]
    pub user: String,
    #[arg(long, short)]
    pub password: String,
    #[arg(long, short)]
    pub socket: Option<u16>,
}

impl Args {
    pub fn get_ip_addr(&self) -> anyhow::Result<IpAddr> {
        match &self.ip_addr {
            SendAddress::Tailscale(name) => get_tailscale_ip(Some(name)),
            SendAddress::Localhost => Ok(IpAddr::V4(Ipv4Addr::LOCALHOST)),
            SendAddress::Ip(ip_addr) => Ok(*ip_addr),
        }
    }
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
enum SendAddress {
    Tailscale(String),
    Localhost,
    Ip(IpAddr),
}

impl FromStr for SendAddress {
    type Err = Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s.to_ascii_lowercase().as_str() {
            "localhost" => Self::Localhost,
            _ => s
                .parse::<IpAddr>()
                .map_or(Self::Tailscale(s.to_string()), Self::Ip),
        })
    }
}
