use std::sync::Arc;

use anyhow::Result;
use clap::Parser;
use crossterm::terminal;
use rc::Args;
use russh::{ChannelMsg, Disconnect, client};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

struct Client;

impl client::Handler for Client {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        _server_public_key: &russh::keys::PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        Ok(true)
    }
}

struct RawTerminal;

impl RawTerminal {
    fn new() -> Result<Self> {
        terminal::enable_raw_mode()?;
        Ok(Self)
    }
}

impl Drop for RawTerminal {
    fn drop(&mut self) {
        let _ = terminal::disable_raw_mode();
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    let mut session = russh::client::connect(
        Arc::new(russh::client::Config::default()),
        (args.ip_addr, args.socket.unwrap_or(2222)),
        Client,
    )
    .await?;

    let auth = session.authenticate_password("demo", "demo").await?;

    if !auth.success() {
        anyhow::bail!("Authentication failed");
    }

    let mut channel = session.channel_open_session().await?;

    let (cols, rows) = crossterm::terminal::size()?;

    channel
        .request_pty(false, "xterm-256color", cols.into(), rows.into(), 0, 0, &[])
        .await?;

    channel.request_shell(true).await?;

    // 6. Enter raw mode so keystrokes are forwarded immediately.
    let _raw_terminal = RawTerminal::new()?;

    let mut stdin = tokio::io::stdin();
    let mut stdout = tokio::io::stdout();
    let mut buffer = [0u8; 4096];
    let mut stdin_closed = false;

    // 7. Forward input and output concurrently.
    loop {
        tokio::select! {
            result = stdin.read(&mut buffer), if !stdin_closed => {
                let n = result?;

                if n == 0 {
                    stdin_closed = true;
                    channel.eof().await?;
                } else {
                    channel.data(&buffer[..n]).await?;
                }
            }

            message = channel.wait() => {
                match message {
                    Some(ChannelMsg::Data { data }) => {
                        stdout.write_all(&data).await?;
                        stdout.flush().await?;
                    }

                    Some(ChannelMsg::ExtendedData { data, .. }) => {
                        stdout.write_all(&data).await?;
                        stdout.flush().await?;
                    }

                    Some(ChannelMsg::Eof | ChannelMsg::Close) | None => {
                        break;
                    }

                    _ => {}
                }
            }
        }
    }

    // 8. Disconnect cleanly.
    session
        .disconnect(Disconnect::ByApplication, "Client closed", "en")
        .await?;

    Ok(())
}
