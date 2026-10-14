pub use russh::server::*;
use tokio_util::sync::CancellationToken;

use std::path::PathBuf;
use std::{collections::HashMap, fmt::Debug, net::IpAddr, sync::atomic::AtomicU64};
use std::{fs, io};

use anyhow::Result;
use russh::{Channel, ChannelId};
use rust_pty::{NativePtySystem, PtyConfigBuilder, PtyError, PtyMaster, PtySystem, WindowSize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;

#[derive(Debug, clap::Parser, serde::Deserialize, serde::Serialize)]
pub struct Args {
    #[arg(long, short)]
    pub log_path: Option<PathBuf>,
    pub ip_addr: IpAddr,
    #[command(subcommand)]
    pub users: UsersArg,
    #[arg(long, short)]
    pub socket: Option<u16>,
}

impl Args {
    pub fn canonicalize(&mut self) -> io::Result<()> {
        if let Some(path) = self.log_path.take() {
            self.log_path = Some(path.canonicalize()?)
        }

        match &mut self.users {
            UsersArg::File { path } => *path = path.clone().canonicalize()?,
            _ => (),
        }

        Ok(())
    }
}

#[derive(Debug, Clone, clap::Subcommand, serde::Deserialize, serde::Serialize)]
pub enum UsersArg {
    /// Input a name and password pair to use
    Manual { name: String, password: String },
    /// Specify user and passwords pairs as coming from this file.
    /// The format is a string delimited by ';' with the user name
    /// and password being whitespace delimited
    File { path: PathBuf },
}

impl UsersArg {
    pub fn to_hash_map(&self) -> io::Result<HashMap<String, String>> {
        let mut map = HashMap::new();

        match self {
            UsersArg::Manual { name, password } => {
                map.insert(name.clone(), password.clone());
            }
            UsersArg::File { path } => {
                fn parse_pasword_pair(s: &str) -> io::Result<(String, String)> {
                    if s.is_empty() {
                        return Err(io::Error::new(
                            io::ErrorKind::UnexpectedEof,
                            "no data to parse",
                        ));
                    }

                    let mut split = s.split_whitespace();

                    let name = split.next().unwrap();

                    let password = split.next().ok_or(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("no password supplied for name `{}`", name),
                    ))?;

                    let _ = split.next().is_none().ok_or(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("only expected two parts, (name & password), got more"),
                    ))?;

                    Ok((name.to_string(), password.to_string()))
                }

                let file_contents = fs::read_to_string(path)?;

                for pair in file_contents.split(';') {
                    let (name, password) = parse_pasword_pair(pair)?;

                    map.insert(name.clone(), password)
                        .ok_or(io::Error::other(format!(
                            "name already has a defined password {name}"
                        )))?;
                }

                assert!(
                    !map.is_empty(),
                    "cannot connect to a server with no user, password pairs"
                );
            }
        };

        Ok(map)
    }
}

pub struct Server {
    pub shutdown: CancellationToken,
    pub user_store_path: PathBuf,
}

impl russh::server::Server for Server {
    type Handler = Handler;

    fn new_client(&mut self, _peer: Option<std::net::SocketAddr>) -> Handler {
        let id = NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Handler {
            shutdown: self.shutdown.child_token(),
            id,
            ..Default::default()
        }
    }
}

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Default)]
pub struct Handler {
    shutdown: CancellationToken,
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

#[derive(Debug)]
struct Pty {
    sender: tokio::sync::mpsc::Sender<PtyCommand>,
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

        self.sender.send(PtyCommand::Close).await?;

        Ok(())
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

impl russh::server::Handler for Handler {
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
        reply: ChannelOpenHandle,
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

        let (mut master, mut child) = match NativePtySystem::spawn_shell(&cfg).await {
            Ok(r) => r,
            Err(e) => {
                session.channel_failure(channel)?;
                return Err(e.into());
            }
        };

        let (tx, mut rx) = mpsc::channel::<PtyCommand>(64);

        let handle = session.handle();

        let id = self.id;

        let shutdown = self.shutdown.clone();

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
                                log::error!("PTY read error: {e} (id: {id})");
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
                                    log::error!("PTY resize error: {e} (id: {id})");
                                }
                            }
                            Some(PtyCommand::Close) | None => {
                                break;
                            },
                        }
                    }

                    _ = shutdown.cancelled() => {
                        break;
                    }
                }
            }

            let _ = master.close();
            let _ = child.kill();

            let _ = handle.eof(channel).await;
            let _ = handle.close(channel).await;

            log::info!("PTY exited (id: {id})");
        });

        self.ptys.insert(self.id, Pty { sender: tx });

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
