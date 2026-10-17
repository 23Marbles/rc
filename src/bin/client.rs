use std::sync::Arc;

use anyhow::Result;
use clap::Parser;
use crossterm::{
    event::{Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    terminal,
};
use futures_util::StreamExt;
use rc::client;
use russh::{ChannelMsg, Disconnect};
use tokio::io::AsyncWriteExt;

struct Client;

fn key_event_to_bytes(key: KeyEvent) -> Option<Vec<u8>> {
    if key.kind == KeyEventKind::Release {
        return None;
    }

    let mut bytes = match key.code {
        KeyCode::Char(character) if key.modifiers.contains(KeyModifiers::CONTROL) => {
            let control = match character {
                'a'..='z' => character as u8 - b'a' + 1,
                'A'..='Z' => character as u8 - b'A' + 1,
                '@' | ' ' => 0,
                '[' => 0x1b,
                '\\' => 0x1c,
                ']' => 0x1d,
                '^' => 0x1e,
                '_' => 0x1f,
                '?' => 0x7f,
                _ => return None,
            };
            vec![control]
        }
        KeyCode::Char(character) => character.to_string().into_bytes(),
        KeyCode::Enter => vec![b'\r'],
        KeyCode::Tab => vec![b'\t'],
        KeyCode::BackTab => b"\x1b[Z".to_vec(),
        KeyCode::Backspace => vec![0x7f],
        KeyCode::Esc => vec![0x1b],
        KeyCode::Up => b"\x1b[A".to_vec(),
        KeyCode::Down => b"\x1b[B".to_vec(),
        KeyCode::Right => b"\x1b[C".to_vec(),
        KeyCode::Left => b"\x1b[D".to_vec(),
        KeyCode::Home => b"\x1b[H".to_vec(),
        KeyCode::End => b"\x1b[F".to_vec(),
        KeyCode::Insert => b"\x1b[2~".to_vec(),
        KeyCode::Delete => b"\x1b[3~".to_vec(),
        KeyCode::PageUp => b"\x1b[5~".to_vec(),
        KeyCode::PageDown => b"\x1b[6~".to_vec(),
        KeyCode::F(1) => b"\x1bOP".to_vec(),
        KeyCode::F(2) => b"\x1bOQ".to_vec(),
        KeyCode::F(3) => b"\x1bOR".to_vec(),
        KeyCode::F(4) => b"\x1bOS".to_vec(),
        KeyCode::F(5) => b"\x1b[15~".to_vec(),
        KeyCode::F(6) => b"\x1b[17~".to_vec(),
        KeyCode::F(7) => b"\x1b[18~".to_vec(),
        KeyCode::F(8) => b"\x1b[19~".to_vec(),
        KeyCode::F(9) => b"\x1b[20~".to_vec(),
        KeyCode::F(10) => b"\x1b[21~".to_vec(),
        KeyCode::F(11) => b"\x1b[23~".to_vec(),
        KeyCode::F(12) => b"\x1b[24~".to_vec(),
        _ => return None,
    };

    if key.modifiers.contains(KeyModifiers::ALT)
        && !matches!(key.code, KeyCode::Esc | KeyCode::BackTab)
    {
        bytes.insert(0, 0x1b);
    }

    Some(bytes)
}

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
    let args = client::Args::parse();

    let mut session = russh::client::connect(
        Arc::new(russh::client::Config::default()),
        (args.get_ip_addr()?, args.socket.unwrap_or(2222)),
        Client,
    )
    .await?;

    let auth = session
        .authenticate_password(args.user, args.password)
        .await?;

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

    let mut stdout = tokio::io::stdout();
    let mut input_closed = false;

    let mut stream = EventStream::new();

    // 7. Forward input and output concurrently.
    loop {
        tokio::select! {
            event = stream.next(), if !input_closed => {
                match event {
                    Some(Ok(Event::Key(key))) => {
                        if let Some(data) = key_event_to_bytes(key) {
                            channel.data(data.as_slice()).await?;
                        }
                    }
                    Some(Ok(Event::Paste(text))) => {
                        channel.data(text.as_bytes()).await?;
                    }
                    Some(Ok(Event::Resize(cols, rows))) => {
                        channel.window_change(cols.into(), rows.into(), 0, 0).await?;
                    }
                    Some(Ok(_)) => {}
                    Some(Err(error)) => return Err(error.into()),
                    None => {
                        input_closed = true;
                        channel.eof().await?;
                    }
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

#[cfg(test)]
mod tests {
    use super::key_event_to_bytes;
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

    #[test]
    fn arrow_keys_are_forwarded_as_terminal_sequences() {
        assert_eq!(
            key_event_to_bytes(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE)),
            Some(b"\x1b[A".to_vec())
        );
        assert_eq!(
            key_event_to_bytes(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE)),
            Some(b"\x1b[D".to_vec())
        );
    }

    #[test]
    fn control_characters_are_forwarded_as_control_bytes() {
        assert_eq!(
            key_event_to_bytes(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Some(vec![0x03])
        );
    }

    #[test]
    fn key_release_events_are_not_forwarded() {
        let mut key = KeyEvent::new(KeyCode::Up, KeyModifiers::NONE);
        key.kind = KeyEventKind::Release;

        assert_eq!(key_event_to_bytes(key), None);
    }
}
