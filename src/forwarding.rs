use crate::{
    net::Ws,
    protocol::{Data, FILE_CHUNK},
};
use anyhow::{Context, Result, bail};
use futures_util::{SinkExt, StreamExt};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    sync::{mpsc, oneshot},
};
use tokio_tungstenite::tungstenite::Message;

pub async fn connect_loopback(port: u16) -> Result<TcpStream> {
    if port == 0 {
        bail!("INVALID_PORT: remote port must be 1..65535")
    }
    let mut error = None;
    for host in ["127.0.0.1", "::1"] {
        match tokio::time::timeout(Duration::from_secs(2), TcpStream::connect((host, port))).await {
            Ok(Ok(stream)) => {
                stream.set_nodelay(true)?;
                return Ok(stream);
            }
            Ok(Err(e)) => error = Some(e.to_string()),
            Err(_) => error = Some("connection timed out".into()),
        }
    }
    bail!(
        "FORWARD_CONNECT_FAILED: localhost:{port}: {}",
        error.unwrap_or_default()
    )
}

// Each direction advances independently, with at most eight 64-KiB messages
// queued. End marks a TCP write-half close, not a WebSocket disconnect.
pub async fn bridge(ws: &mut Ws, tcp: TcpStream) -> Result<()> {
    tcp.set_nodelay(true)?;
    let (mut socket_tx, mut socket_rx) = ws.split();
    let (mut input, mut output) = tcp.into_split();
    let (tx, mut rx) = mpsc::channel::<Message>(8);
    let input_tx = tx.clone();
    let (ack_tx, ack_rx) = oneshot::channel();
    let sent_eof = Arc::new(AtomicBool::new(false));
    let sender_eof = sent_eof.clone();
    let send = async move {
        let mut buffer = vec![0; FILE_CHUNK];
        loop {
            let n = input.read(&mut buffer).await?;
            let message = if n == 0 {
                sender_eof.store(true, Ordering::Relaxed);
                Message::Text(serde_json::to_string(&Data::End)?.into())
            } else {
                Message::Binary(buffer[..n].to_vec().into())
            };
            input_tx
                .send(message)
                .await
                .context("CONNECTION_CLOSED: forwarding writer stopped")?;
            if n == 0 {
                ack_rx
                    .await
                    .context("CONNECTION_CLOSED: peer did not acknowledge forwarding EOF")?;
                return Ok::<_, anyhow::Error>(());
            }
        }
    };
    let receive = async move {
        let mut received_eof = false;
        let mut acknowledged = false;
        let mut ack_tx = Some(ack_tx);
        while let Some(message) = socket_rx.next().await {
            match message? {
                Message::Binary(bytes) if !received_eof && bytes.len() <= FILE_CHUNK => {
                    output.write_all(&bytes).await?
                }
                Message::Text(text) => match serde_json::from_str::<Data>(&text)? {
                    Data::End if !received_eof => {
                        output.shutdown().await?;
                        received_eof = true;
                        tx.send(Message::Text(
                            serde_json::to_string(&Data::ForwardEofAck)?.into(),
                        ))
                        .await
                        .context("CONNECTION_CLOSED: forwarding writer stopped")?;
                    }
                    Data::ForwardEofAck if !acknowledged && sent_eof.load(Ordering::Relaxed) => {
                        acknowledged = true;
                        let _ = ack_tx.take().unwrap().send(());
                    }
                    Data::Error { code, message } => bail!("{code}: {message}"),
                    _ => bail!("INVALID_MESSAGE: expected forwarding EOF"),
                },
                Message::Ping(bytes) => tx
                    .send(Message::Pong(bytes))
                    .await
                    .context("CONNECTION_CLOSED: forwarding writer stopped")?,
                Message::Pong(_) => {}
                Message::Close(_) => break,
                _ => bail!("INVALID_MESSAGE: forwarding chunk exceeds limit"),
            }
            if received_eof && acknowledged {
                return Ok::<_, anyhow::Error>(());
            }
        }
        bail!("CONNECTION_CLOSED: forwarded TCP connection interrupted")
    };
    let writer = async move {
        let mut ping = tokio::time::interval(Duration::from_secs(15));
        loop {
            let message = tokio::select! {
                m = rx.recv() => match m { Some(m) => m, None => return Ok::<_, anyhow::Error>(()) },
                _ = ping.tick() => Message::Ping(vec![].into()),
            };
            tokio::time::timeout(Duration::from_secs(300), socket_tx.send(message))
                .await
                .context("FORWARD_TIMEOUT: receiver is not reading")??;
        }
    };
    tokio::try_join!(send, receive, writer)?;
    Ok(())
}
