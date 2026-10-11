use std::{collections::HashMap, fmt::Debug, sync::Arc};

use anyhow::Result;
use russh::{
    Channel, ChannelId,
    keys::{Algorithm, PrivateKey},
    server::{self, Auth, Msg, Server as _, Session},
};
use rust_pty::{
    NativePtySystem, PtyChild, PtyConfigBuilder, PtyError, PtyMaster, PtySystem, WindowSize,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::mpsc;

struct Server;

impl server::Server for Server {
    type Handler = Handler;

    fn new_client(&mut self, _peer: Option<std::net::SocketAddr>) -> Handler {
        Handler::default()
    }
}

#[derive(Debug, Default)]
struct Handler {
    requested_ptys: HashMap<ChannelId, PtyRequest>,
    ptys: HashMap<ChannelId, Pty>,
}

#[derive(Debug)]
struct PtyRequest {
    _term: String,
    cols: u32,
    rows: u32,
    _pixel_width: u32,
    _pixel_height: u32,
    _modes: Vec<(russh::Pty, u32)>,
}

struct Pty {
    sender: tokio::sync::mpsc::Sender<PtyCommand>,
    child: Box<dyn PtyChild>,
}

impl Pty {
    async fn write(&self, data: Vec<u8>) -> Result<(), mpsc::error::SendError<PtyCommand>> {
        self.sender.send(PtyCommand::Input(data)).await
    }

    async fn resize(&self, size: WindowSize) -> Result<(), mpsc::error::SendError<PtyCommand>> {
        self.sender.send(PtyCommand::Resize(size)).await
    }

    async fn close(&mut self) -> Result<(), ServerError> {
        if self.child.is_running() {
            self.sender.send(PtyCommand::Close).await?;
            self.child.kill()?;
        }

        Ok(())
    }
}

impl Debug for Pty {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pty")
            .field("sender", &self.sender)
            .finish_non_exhaustive()
    }
}

enum PtyCommand {
    Input(Vec<u8>),
    Resize(WindowSize),
    Close,
}

#[derive(Debug, thiserror::Error)]
enum ServerError {
    #[error(transparent)]
    Russh(#[from] russh::Error),
    #[error("the channel did not have a pty request (id: `{channel}`)")]
    NoPtyRequest { channel: ChannelId },
    #[error("the channel did not have a pty to communicate with (id: `{channel}`)")]
    NoPty { channel: ChannelId },
    #[error(transparent)]
    PtyErr(#[from] PtyError),
    #[error(transparent)]
    IoErr(#[from] std::io::Error),
    #[error(transparent)]
    ChannelSendErr(#[from] mpsc::error::SendError<PtyCommand>),
}

impl server::Handler for Handler {
    type Error = ServerError;

    async fn auth_password(&mut self, user: &str, password: &str) -> Result<Auth, Self::Error> {
        // Development-only credentials.
        Ok(if user == "demo" && password == "demo" {
            Auth::Accept
        } else {
            Auth::Reject {
                proceed_with_methods: None,
                partial_success: false,
            }
        })
    }

    async fn channel_open_session(
        &mut self,
        channel: Channel<Msg>,
        reply: server::ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        reply.accept().await;
        let _ = channel;
        Ok(())
    }

    async fn pty_request(
        &mut self,
        channel: ChannelId,
        term: &str,
        cols: u32,
        rows: u32,
        pixel_width: u32,
        pixel_height: u32,
        modes: &[(russh::Pty, u32)],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        self.requested_ptys.insert(
            channel,
            PtyRequest {
                _term: term.to_string(),
                cols,
                rows,
                _pixel_width: pixel_width,
                _pixel_height: pixel_height,
                _modes: modes.to_vec(),
            },
        );

        session.channel_success(channel)?;
        Ok(())
    }

    async fn shell_request(
        &mut self,
        channel: ChannelId,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        let request = match self
            .requested_ptys
            .remove(&channel)
            .ok_or(ServerError::NoPtyRequest { channel })
        {
            Ok(r) => r,
            Err(e) => {
                session.channel_failure(channel)?;
                return Err(e);
            }
        };

        let cfg = PtyConfigBuilder::new()
            .window_size(request.cols as u16, request.rows as u16)
            .build();

        let (mut master, child) = match NativePtySystem::spawn_shell(&cfg).await {
            Ok(r) => r,
            Err(e) => {
                session.channel_failure(channel)?;
                return Err(e.into());
            }
        };

        let (tx, mut rx) = mpsc::channel::<PtyCommand>(64);

        let handle = session.handle();

        tokio::spawn(async move {
            let mut buffer = [0u8; 8192];

            loop {
                tokio::select! {
                    result = master.read(&mut buffer) => {
                        match result {
                            Ok(0) => break,
                            Ok(n) => {
                                if handle.data(channel, buffer[..n].to_vec()).await.is_err() {
                                    break;
                                }
                            }
                            Err(e) => {
                                eprintln!("PTY read error: {e}");
                                break;
                            }
                        }
                    }

                    command = rx.recv() => {
                        match command {
                            Some(PtyCommand::Input(data)) => {
                                if master.write_all(&data).await.is_err() {
                                    break;
                                }
                            }
                            Some(PtyCommand::Resize(size)) => {
                                if let Err(e) = master.resize(size) {
                                    eprintln!("PTY resize error: {e}");
                                }
                            }
                            Some(PtyCommand::Close) | None => break,
                        }
                    }
                }
            }

            let _ = handle.eof(channel).await;
            let _ = handle.close(channel).await;
        });

        self.ptys.insert(
            channel,
            Pty {
                sender: tx,
                child: Box::new(child),
            },
        );
        session.channel_success(channel)?;
        Ok(())
    }

    async fn data(
        &mut self,
        channel: ChannelId,
        data: &[u8],
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        self.ptys
            .get(&channel)
            .ok_or(ServerError::NoPty { channel })?
            .write(data.to_vec())
            .await?;

        Ok(())
    }

    async fn window_change_request(
        &mut self,
        channel: ChannelId,
        col_width: u32,
        row_height: u32,
        pix_width: u32,
        pix_height: u32,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        let pty = self
            .ptys
            .get(&channel)
            .ok_or(ServerError::NoPty { channel })?;

        match pty
            .resize(WindowSize {
                cols: col_width as u16,
                rows: row_height as u16,
                xpixel: pix_width as u16,
                ypixel: pix_height as u16,
            })
            .await
        {
            Ok(()) => (),
            Err(e) => {
                session.channel_failure(channel)?;
                return Err(e.into());
            }
        };

        session.channel_success(channel)?;

        Ok(())
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let config = server::Config {
        keys: vec![PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519)?],
        ..Default::default()
    };

    let config = Arc::new(config);
    let listener = TcpListener::bind("127.0.0.1:2222").await?;

    println!("Listening on 127.0.0.1:2222");

    let mut server = Server;
    server.run_on_socket(config, &listener).await?;

    Ok(())
}
