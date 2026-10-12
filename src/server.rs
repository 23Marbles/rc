use std::{collections::HashMap, fmt::Debug, sync::atomic::AtomicU64};

use anyhow::Result;
use russh::{
    Channel, ChannelId,
    server::{self, Auth, Msg, Session},
};
use rust_pty::{
    NativePtySystem, PtyChild, PtyConfigBuilder, PtyError, PtyMaster, PtySystem, WindowSize,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;

pub struct Server;

impl server::Server for Server {
    type Handler = Handler;

    fn new_client(&mut self, _peer: Option<std::net::SocketAddr>) -> Handler {
        let id = NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Handler {
            id,
            ..Default::default()
        }
    }
}

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Default)]
pub struct Handler {
    id: u64,
    requested_ptys: HashMap<u64, PtyRequest>,
    ptys: HashMap<u64, Pty>,
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
    pub async fn write(&self, data: Vec<u8>) -> Result<(), mpsc::error::SendError<PtyCommand>> {
        self.sender.send(PtyCommand::Input(data)).await
    }

    pub async fn resize(&self, size: WindowSize) -> Result<(), mpsc::error::SendError<PtyCommand>> {
        self.sender.send(PtyCommand::Resize(size)).await
    }

    pub async fn close(&mut self) -> Result<(), ServerError> {
        log::info!("Closing pty");

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

pub enum PtyCommand {
    Input(Vec<u8>),
    Resize(WindowSize),
    Close,
}

#[derive(Debug, thiserror::Error)]
pub enum ServerError {
    #[error(transparent)]
    Russh(#[from] russh::Error),
    #[error("the handle did not have a pty request (id: `{id}`)")]
    NoPtyRequest { id: u64 },
    #[error("the handle did not have a pty to communicate with (id: `{id}`)")]
    NoPty { id: u64 },
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
            log::info!("User authenticated");

            Auth::Accept
        } else {
            log::warn!("User failed authentication");

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
        log::info!("Channel open session");

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
        log::info!("PTY requested (id: {})", self.id);

        self.requested_ptys.insert(
            self.id,
            PtyRequest {
                _term: term.to_string(),
                cols,
                rows,
                _pixel_width: pixel_width,
                _pixel_height: pixel_height,
                _modes: modes.to_vec(),
            },
        );

        log::info!("PTY aquired successfully (id: {})", self.id);

        session.channel_success(channel)?;
        Ok(())
    }

    async fn shell_request(
        &mut self,
        channel: ChannelId,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        log::info!("channel requesting shell (id: {})", self.id);

        let request = match self
            .requested_ptys
            .remove(&self.id)
            .ok_or(ServerError::NoPtyRequest { id: self.id })
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

        let id = self.id;

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
                                log::error!("PTY read error: {e} (id: {})", id);
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
                                    log::error!("PTY resize error: {e} (id: {})", id);
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
            self.id,
            Pty {
                sender: tx,
                child: Box::new(child),
            },
        );

        log::info!("channel successfully aquired shell (id: {})", self.id);

        session.channel_success(channel)?;
        Ok(())
    }

    async fn data(
        &mut self,
        _channel: ChannelId,
        data: &[u8],
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        self.ptys
            .get(&self.id)
            .ok_or(ServerError::NoPty { id: self.id })?
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
            .get(&self.id)
            .ok_or(ServerError::NoPty { id: self.id })?;

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

        log::trace!(
            "Window resize {}x{} (id: {})",
            row_height,
            col_width,
            self.id
        );

        session.channel_success(channel)?;

        Ok(())
    }
}
