//! Resumable file windows. Acknowledgments cover only fsynced and checkpointed plaintext.
use super::*;
use std::io::{Read, Seek, SeekFrom};
const WINDOW: u64 = 1024 * 1024;
fn prefix(file: &std::fs::File, offset: u64) -> Result<Sha256> {
    let mut file = file.try_clone()?;
    file.seek(SeekFrom::Start(0))?;
    let mut hash = Sha256::new();
    let mut left = offset;
    let mut buffer = [0; FILE_CHUNK];
    while left > 0 {
        let n = file.read(&mut buffer[..left.min(FILE_CHUNK as u64) as usize])?;
        if n == 0 {
            bail!(ErrorCode::ChecksumMismatch.error("saved transfer prefix is incomplete"));
        }
        hash.update(&buffer[..n]);
        left -= n as u64;
    }
    Ok(hash)
}
pub(crate) fn prefix_hash(file: &std::fs::File, offset: u64) -> Result<String> {
    Ok(hex::encode(prefix(file, offset)?.finalize()))
}
pub(crate) async fn send_remaining(
    ws: &mut Ws,
    file: &std::fs::File,
    size: u64,
    mut offset: u64,
    mut checkpoint: impl FnMut(u64) -> Result<()>,
) -> Result<()> {
    if offset > size {
        bail!(ErrorCode::InvalidBody.error("resume offset exceeds content"));
    }
    let mut file = tokio::fs::File::from_std(file.try_clone()?);
    tokio::io::AsyncSeekExt::seek(&mut file, SeekFrom::Start(offset)).await?;
    let mut bytes = vec![0; FILE_CHUNK];
    while offset < size {
        let end = (offset.saturating_add(WINDOW)).min(size);
        while offset < end {
            let n = file
                .read(&mut bytes[..(end - offset).min(FILE_CHUNK as u64) as usize])
                .await?;
            if n == 0 {
                bail!(
                    ErrorCode::ChecksumMismatch.error("immutable transfer snapshot is truncated")
                );
            }
            send_chunk(ws, &bytes[..n]).await?;
            offset += n as u64;
        }
        ws.flush().await?;
        let Data::TransferAck { offset: confirmed } = receive(ws).await? else {
            bail!(ErrorCode::InvalidMessage.error("expected durable transfer acknowledgment"));
        };
        if confirmed != offset {
            bail!(
                ErrorCode::InvalidMessage.error("transfer acknowledgment differs from the window")
            );
        }
        checkpoint(offset)?;
    }
    send(ws, &Data::End).await
}
pub(crate) async fn receive_remaining(
    ws: &mut Ws,
    file: &std::fs::File,
    size: u64,
    hash: &str,
    mut offset: u64,
    mut checkpoint: impl FnMut(u64) -> Result<()>,
) -> Result<()> {
    if offset > size {
        bail!(ErrorCode::InvalidBody.error("resume offset exceeds content"));
    }
    let mut digest = prefix(file, offset)?;
    file.set_len(offset)?;
    let mut output = tokio::fs::File::from_std(file.try_clone()?);
    tokio::io::AsyncSeekExt::seek(&mut output, SeekFrom::Start(offset)).await?;
    let mut acked = offset;
    loop {
        match ws
            .next()
            .await
            .context(ErrorCode::ConnectionClosed.error("transfer interrupted"))??
        {
            Message::Binary(frame) => {
                let bytes = encoding::decode(&frame, FILE_CHUNK)?;
                if bytes.is_empty() || bytes.len() as u64 > size - offset {
                    bail!(ErrorCode::InvalidBody.error("unexpected resumable chunk size"));
                }
                output.write_all(&bytes).await?;
                digest.update(&bytes);
                offset += bytes.len() as u64;
                if offset - acked >= WINDOW || offset == size {
                    if offset == size && hex::encode(digest.clone().finalize()) != hash {
                        bail!(ErrorCode::ChecksumMismatch.error("final transfer digest differs"));
                    }
                    output.flush().await?;
                    output.sync_all().await?;
                    checkpoint(offset)?;
                    send(ws, &Data::TransferAck { offset }).await?;
                    acked = offset;
                }
            }
            Message::Text(text) if matches!(serde_json::from_str::<Data>(&text)?, Data::End) => {
                break;
            }
            Message::Ping(bytes) => ws.send(Message::Pong(bytes)).await?,
            Message::Pong(_) => {}
            Message::Close(_) => bail!(ErrorCode::ConnectionClosed.error("transfer interrupted")),
            _ => bail!(ErrorCode::InvalidBody.error("expected transfer chunks or end")),
        }
    }
    if offset != size || hex::encode(digest.finalize()) != hash {
        bail!(ErrorCode::ChecksumMismatch.error("resumed content digest differs"));
    }
    output.flush().await?;
    output.sync_all().await?;
    checkpoint(offset)?;
    Ok(())
}
