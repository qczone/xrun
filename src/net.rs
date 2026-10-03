use crate::{
    config::{Identity, atomic_private_write, device_dir},
    crypto,
    protocol::*,
};
use anyhow::{Context, Result, bail};
use futures_util::{SinkExt, StreamExt};
use serde::{Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::TcpStream,
};
use tokio_tungstenite::{
    WebSocketStream, client_async_with_config,
    tungstenite::{
        Message, client::IntoClientRequest, http::HeaderValue, protocol::WebSocketConfig,
    },
};
pub trait Transport: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Transport for T {}
pub type Io = Box<dyn Transport>;
pub type Ws = WebSocketStream<Io>;
pub async fn tcp(url: &url::Url) -> Result<TcpStream> {
    let host = url.host_str().context("missing host")?;
    let port = url.port_or_known_default().context("missing port")?;
    let addresses = tokio::net::lookup_host((host, port)).await?;
    let mut error = None;
    for addr in addresses.filter(|a| a.is_ipv4()) {
        match TcpStream::connect(addr).await {
            Ok(tcp) => {
                tcp.set_nodelay(true)?;
                return Ok(tcp);
            }
            Err(e) => error = Some(e),
        }
    }
    bail!(
        "CONNECT_FAILED: {}",
        error
            .map(|e| e.to_string())
            .unwrap_or_else(|| "host has no IPv4 A record".into())
    )
}
pub fn ordered(id: &Identity) -> Vec<String> {
    ordered_addresses(
        &id.addresses,
        &crypto::ca_spki_pin(&id.ca_pem).unwrap_or_default(),
    )
}
pub fn ordered_addresses(values: &[String], pin: &str) -> Vec<String> {
    let mut addresses = values.to_vec();
    if let Ok(path) = device_dir().map(|p| p.join("last-address.json"))
        && let Ok(bytes) = std::fs::read(path)
        && let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes)
        && value["ca_pin"].as_str() == Some(pin)
        && let Some(addr) = value["address"].as_str()
        && let Some(i) = addresses.iter().position(|a| a == addr)
    {
        let a = addresses.remove(i);
        addresses.insert(0, a)
    }
    addresses
}
pub(crate) fn remember(id: &Identity, address: &str) {
    if let Ok(dir) = device_dir() {
        let _=atomic_private_write(&dir.join("last-address.json"),&serde_json::to_vec(&serde_json::json!({"ca_pin":crypto::ca_spki_pin(&id.ca_pem).ok(),"address":address})).unwrap());
    }
}
pub async fn websocket_at(address: &str, path: &str, tls: Arc<rustls::ClientConfig>) -> Result<Ws> {
    let url = url::Url::parse(&format!(
        "{}{}",
        address.replace("https://", "wss://"),
        path
    ))?;
    let tcp = tcp(&url).await?;
    let mut request = url.as_str().into_client_request()?;
    request
        .headers_mut()
        .insert("x-xrun-version", HeaderValue::from_static(VERSION));
    let config = WebSocketConfig::default()
        .max_message_size(Some(MAX_MESSAGE))
        .max_frame_size(Some(MAX_MESSAGE));
    let host = url.host_str().context("missing host")?.to_owned();
    let tls = tokio_rustls::TlsConnector::from(tls)
        .connect(rustls::pki_types::ServerName::try_from(host)?, tcp)
        .await?;
    let result = client_async_with_config(request, Box::new(tls) as Io, Some(config)).await;
    match result {
        Ok((ws, _)) => Ok(ws),
        Err(tokio_tungstenite::tungstenite::Error::Http(r)) => {
            if let Some(body) = r.body()
                && let Ok(Data::Error { code, message }) = serde_json::from_slice(body)
            {
                bail!("{code}: {message}")
            }
            bail!("SESSION_REJECTED: {}", r.status())
        }
        Err(e) => Err(e.into()),
    }
}
pub async fn websocket(id: &Identity, path: &str) -> Result<(Ws, String)> {
    let target = path
        .strip_prefix("/devices/")
        .and_then(|p| p.strip_suffix("/session"))
        .context("INVALID_REQUEST: expected a device session")?;
    crate::network::session(id, target).await
}
pub fn explicit(e: &anyhow::Error) -> bool {
    let s = e.to_string();
    [
        "DEVICE_REVOKED",
        "VERSION_MISMATCH",
        "SOURCE_NOT_ALLOWED",
        "ACCESS_PAUSED",
        "DEVICE_OFFLINE",
        "SESSION_LIMIT",
        "NOT_MANAGER",
        "MIGRATION_REQUIRED",
        "IDENTITY_MISMATCH",
        "UNKNOWN_DEVICE",
        "INVALID_TOKEN",
        "INVALID_NAME",
        "NAME_TAKEN",
        "MEMBER_LIMIT",
    ]
    .iter()
    .any(|c| s.starts_with(c))
}
pub async fn http<T: DeserializeOwned>(
    id: &Identity,
    method: reqwest::Method,
    path: &str,
    body: Option<serde_json::Value>,
) -> Result<T> {
    crate::network::http(id, method, path, body).await
}
pub async fn send<T: Serialize>(ws: &mut Ws, value: &T) -> Result<()> {
    let bytes = serde_json::to_string(value)?;
    if bytes.len() > MAX_MESSAGE {
        bail!("MESSAGE_TOO_LARGE: {}", bytes.len())
    }
    ws.send(Message::Text(bytes.into())).await?;
    Ok(())
}
pub async fn receive<T: DeserializeOwned>(ws: &mut Ws) -> Result<T> {
    loop {
        match ws
            .next()
            .await
            .context("CONNECTION_CLOSED: peer closed connection")??
        {
            Message::Text(s) => return Ok(serde_json::from_str(&s)?),
            Message::Ping(bytes) => ws.send(Message::Pong(bytes)).await?,
            Message::Pong(_) => {}
            Message::Close(_) => bail!("CONNECTION_CLOSED: peer closed connection"),
            _ => bail!("INVALID_MESSAGE: expected JSON"),
        }
    }
}
pub async fn send_bytes(ws: &mut Ws, bytes: &[u8]) -> Result<()> {
    for chunk in bytes.chunks(FILE_CHUNK) {
        ws.send(Message::Binary(chunk.to_vec().into())).await?
    }
    send(ws, &Data::End).await
}
pub async fn receive_bytes(ws: &mut Ws, size: u64, hash: &str, max: u64) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    receive_body(ws, &mut bytes, size, hash, max).await?;
    Ok(bytes)
}
pub async fn receive_file(ws: &mut Ws, size: u64, hash: &str) -> Result<tempfile::NamedTempFile> {
    let temp = tempfile::Builder::new().prefix("xrun-upload-").tempfile()?;
    let mut file = tokio::fs::File::from_std(temp.reopen()?);
    receive_body(ws, &mut file, size, hash, MAX_FILE).await?;
    file.flush().await?;
    Ok(temp)
}
pub async fn send_file(ws: &mut Ws, file: &std::fs::File) -> Result<()> {
    let mut file = tokio::fs::File::from_std(file.try_clone()?);
    let mut chunk = vec![0; FILE_CHUNK];
    loop {
        let n = file.read(&mut chunk).await?;
        if n == 0 {
            break;
        }
        ws.send(Message::Binary(chunk[..n].to_vec().into())).await?;
    }
    send(ws, &Data::End).await
}
async fn receive_body<W: AsyncWrite + Unpin>(
    ws: &mut Ws,
    output: &mut W,
    size: u64,
    hash: &str,
    max: u64,
) -> Result<()> {
    if size > max {
        bail!("FILE_TOO_LARGE: limit {max} bytes")
    }
    let mut received = 0u64;
    let mut digest = Sha256::new();
    loop {
        match ws
            .next()
            .await
            .context("CONNECTION_CLOSED: incomplete body")??
        {
            Message::Binary(chunk) => {
                if chunk.len() > FILE_CHUNK || received + chunk.len() as u64 > size {
                    bail!("INVALID_BODY: unexpected chunk size")
                }
                output.write_all(&chunk).await?;
                digest.update(&chunk);
                received += chunk.len() as u64;
            }
            Message::Text(text) => match serde_json::from_str::<Data>(&text)? {
                Data::End => break,
                Data::Error { code, message } => bail!("{code}: {message}"),
                _ => bail!("INVALID_BODY: expected end"),
            },
            Message::Ping(b) => ws.send(Message::Pong(b)).await?,
            Message::Pong(_) => {}
            _ => bail!("CONNECTION_CLOSED: incomplete body"),
        }
    }
    if received != size || hex::encode(digest.finalize()) != hash {
        bail!("CHECKSUM_MISMATCH: incomplete or corrupted body")
    }
    Ok(())
}
pub async fn renew_identity(id: &mut Identity) -> Result<()> {
    crate::network::renew(id).await
}
