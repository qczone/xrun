use crate::{
    config::{Identity, atomic_private_write, device_dir},
    crypto,
    protocol::*,
};
use anyhow::{Context, Result, bail};
use futures_util::{SinkExt, StreamExt};
use serde::{Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};
use std::{sync::Arc, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::TcpStream,
    time::timeout,
};
use tokio_tungstenite::{
    Connector, MaybeTlsStream, WebSocketStream, client_async_tls_with_config,
    tungstenite::{
        Message, client::IntoClientRequest, http::HeaderValue, protocol::WebSocketConfig,
    },
};
pub type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;
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
    let result =
        client_async_tls_with_config(request, tcp, Some(config), Some(Connector::Rustls(tls)))
            .await;
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
    let tls = crypto::client_tls_config(id)?;
    let mut error = None;
    for address in ordered(id) {
        match timeout(
            Duration::from_secs(5),
            websocket_at(&address, path, tls.clone()),
        )
        .await
        {
            Ok(Ok(ws)) => {
                remember(id, &address);
                return Ok((ws, address));
            }
            Ok(Err(e)) => {
                if explicit(&e) {
                    return Err(e);
                }
                error = Some(e)
            }
            Err(_) => error = Some(anyhow::anyhow!("CONNECT_TIMEOUT: {address}")),
        }
    }
    Err(error.unwrap_or_else(|| anyhow::anyhow!("CONNECT_FAILED: no configured address")))
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
        "NOT_ADMIN",
        "UNKNOWN_DEVICE",
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
    let client = crypto::http_client(&id.ca_pem, Some(id))?;
    let mut error = None;
    for address in ordered(id) {
        let mut r = client
            .request(method.clone(), format!("{address}{path}"))
            .header("x-xrun-version", VERSION);
        if let Some(body) = &body {
            r = r.json(body)
        }
        match timeout(Duration::from_secs(5), r.send()).await {
            Ok(Ok(response)) => {
                remember(id, &address);
                if !response.status().is_success() {
                    let message = response.text().await?;
                    if let Ok(Data::Error { code, message }) = serde_json::from_str(&message) {
                        bail!("{code}: {message}")
                    }
                    bail!("HTTP_ERROR: {message}")
                }
                return Ok(response.json().await?);
            }
            Ok(Err(e)) => error = Some(e.into()),
            Err(_) => error = Some(anyhow::anyhow!("CONNECT_TIMEOUT: {address}")),
        }
    }
    Err(error.unwrap_or_else(|| anyhow::anyhow!("CONNECT_FAILED: no configured address")))
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
    if !crypto::certificate_expiring(&id.cert_pem, 365)? {
        return Ok(());
    }
    let client = crypto::http_client(&id.ca_pem, None)?;
    let csr = crypto::renew_device_request(&id.key_pem)?;
    use base64::{Engine, engine::general_purpose::STANDARD};
    let body = PairRequest {
        token: String::new(),
        name: id.name.clone(),
        csr_base64: STANDARD.encode(csr),
    };
    let mut error = None;
    for address in ordered(id) {
        match timeout(
            Duration::from_secs(5),
            client
                .post(format!("{address}/pair"))
                .header("x-xrun-version", VERSION)
                .json(&body)
                .send(),
        )
        .await
        {
            Ok(Ok(r)) => {
                if !r.status().is_success() {
                    bail!("RENEW_FAILED: {}", r.text().await?)
                }
                let pair: PairResponse = r.json().await?;
                if pair.device_id != id.device_id {
                    bail!("IDENTITY_MISMATCH: renewal returned a different device")
                };
                id.cert_pem = pair.cert_pem;
                id.save()?;
                return Ok(());
            }
            Ok(Err(e)) => error = Some(e.into()),
            Err(_) => error = Some(anyhow::anyhow!("CONNECT_TIMEOUT: {address}")),
        }
    }
    Err(error.unwrap_or_else(|| anyhow::anyhow!("CONNECT_FAILED: renewal unavailable")))
}
