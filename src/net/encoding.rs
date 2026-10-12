//! Independent zstd frames, with bounded plaintext, window and exact digest verification.
use crate::{
    error::ErrorCode,
    protocol::{Compression, MAX_MESSAGE},
};
use anyhow::{Result, bail};
use sha2::{Digest, Sha256};
pub(super) const HEADER: usize = 41;
fn already_compressed(bytes: &[u8]) -> bool {
    [
        b"\x89PNG".as_slice(),
        b"\xff\xd8\xff",
        b"PK\x03\x04",
        b"\x1f\x8b",
        b"\x28\xb5\x2f\xfd",
    ]
    .iter()
    .any(|prefix| bytes.starts_with(prefix))
        || (bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP"))
        || bytes.get(4..8) == Some(b"ftyp")
}
pub(super) fn encode(bytes: &[u8], policy: Compression) -> Result<Vec<u8>> {
    if bytes.len() > MAX_MESSAGE - HEADER {
        bail!(ErrorCode::MessageTooLarge.error("plaintext chunk too large"));
    }
    let compressed = if bytes.len() >= 256
        && policy != Compression::None
        && (policy == Compression::Zstd || !already_compressed(bytes))
    {
        let mut encoder = zstd::bulk::Compressor::new(1)?;
        encoder.set_parameter(zstd::zstd_safe::CParameter::WindowLog(20))?;
        let output = encoder.compress(bytes)?;
        let required = if policy == Compression::Auto {
            bytes.len() / 8
        } else {
            1
        };
        (output.len() + required < bytes.len()).then_some(output)
    } else {
        None
    };
    let mut frame = Vec::with_capacity(HEADER + compressed.as_ref().map_or(bytes.len(), Vec::len));
    frame.extend_from_slice(b"XRB1");
    frame.push(u8::from(compressed.is_some()));
    frame.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
    frame.extend_from_slice(&Sha256::digest(bytes));
    frame.extend_from_slice(compressed.as_deref().unwrap_or(bytes));
    Ok(frame)
}
pub(super) fn decode(frame: &[u8], max: usize) -> Result<Vec<u8>> {
    if frame.len() < HEADER || frame.len() > MAX_MESSAGE || &frame[..4] != b"XRB1" {
        bail!(ErrorCode::InvalidBody.error("invalid encoded chunk header"));
    }
    let length = u32::from_be_bytes(frame[5..9].try_into()?) as usize;
    if length > max || length > MAX_MESSAGE - HEADER {
        bail!(ErrorCode::InvalidBody.error("decoded chunk exceeds its bound"));
    }
    let body = &frame[HEADER..];
    let bytes = match frame[4] {
        0 if body.len() == length => body.to_vec(),
        1 if body.len() < length => {
            let mut decoder = zstd::bulk::Decompressor::new()?;
            decoder.window_log_max(20)?;
            decoder.decompress(body, length).map_err(|error| {
                ErrorCode::InvalidBody.error(format!("invalid compressed chunk: {error}"))
            })?
        }
        _ => bail!(ErrorCode::InvalidBody.error("unsupported codec or encoded length")),
    };
    if bytes.len() != length || Sha256::digest(&bytes).as_slice() != &frame[9..41] {
        bail!(ErrorCode::ChecksumMismatch.error("encoded chunk digest differs"));
    }
    Ok(bytes)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn independent_text_chunks_save_bandwidth_and_binary_content_is_preserved() -> Result<()> {
        let text = b"{\"result\":\"build successful\"}\n".repeat(2000);
        let frame = encode(&text, Compression::Auto)?;
        assert_eq!(frame[4], 1);
        assert!(frame.len() < text.len() / 10);
        assert_eq!(decode(&frame, 65536)?, text);
        let raw = encode(&text, Compression::None)?;
        assert_eq!(raw[4], 0);
        assert_eq!(decode(&raw, 65536)?, text);
        let mut png = b"\x89PNG".to_vec();
        png.extend_from_slice(&text);
        assert_eq!(encode(&png, Compression::Auto)?[4], 0);
        let mut random = vec![0; 65536];
        getrandom::fill(&mut random).map_err(|error| anyhow::anyhow!("{error}"))?;
        assert_eq!(encode(&random, Compression::Auto)?[4], 0);
        Ok(())
    }
    #[test]
    fn decoded_size_checksum_and_codec_are_checked_before_publication() -> Result<()> {
        let frame = encode(&vec![b'x'; 65536], Compression::Auto)?;
        assert!(decode(&frame, 1024).is_err());
        let mut invalid = frame.clone();
        invalid[4] = 7;
        assert!(decode(&invalid, 65536).is_err());
        let mut invalid = frame.clone();
        invalid[9] ^= 1;
        assert!(decode(&invalid, 65536).is_err());
        let mut invalid = frame.clone();
        invalid[5..9].copy_from_slice(&1_u32.to_be_bytes());
        assert!(decode(&invalid, 65536).is_err());
        assert!(decode(&frame[..40], 65536).is_err());
        Ok(())
    }
}
