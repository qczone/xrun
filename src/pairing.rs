//! Short invitations: public rendezvous identifiers and endpoint-only secrets.
pub(crate) mod channel;
mod invitations;
use crate::{error::ErrorCode, protocol::RelayMessage};
use anyhow::{Context, Result, bail};
pub(crate) use invitations::Invitations;
use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;

pub(crate) const ALPHABET: &[u8; 32] = b"23456789ABCDEFGHJKLMNPQRSTUVWXYZ";
pub(crate) const MAX_ATTEMPTS: i64 = 5;
pub(crate) const MAX_FRAME: usize = 16 * 1024;
pub(crate) const LIFETIME_MS: i64 = 600_000;
pub(crate) const PUBLIC_PATH: &str = "/.well-known/xrun/pair/";

pub(crate) struct Command {
    pub(crate) message: RelayMessage,
}
pub(crate) type Reply = oneshot::Sender<Result<()>>;

pub(crate) fn random_chars(length: usize) -> String {
    let mut bytes = vec![0; length];
    getrandom::fill(&mut bytes).expect("OS random source");
    bytes
        .into_iter()
        .map(|b| ALPHABET[(b & 31) as usize] as char)
        .collect()
}
pub(crate) fn normalize(code: &str) -> Result<String> {
    if code.len() > 32 {
        bail!(ErrorCode::InvalidCode.error("expected an eight-character access code"));
    }
    let value: String = code
        .chars()
        .filter(|c| !c.is_ascii_whitespace() && *c != '-')
        .map(|c| c.to_ascii_uppercase())
        .collect();
    if value.len() != 8 || !value.bytes().all(|b| ALPHABET.contains(&b)) {
        bail!(ErrorCode::InvalidCode.error("expected an eight-character access code"));
    }
    Ok(value)
}
pub(crate) fn valid_locator(value: &str) -> bool {
    value.len() == 2 && value.bytes().all(|b| ALPHABET.contains(&b))
}
pub(crate) fn valid_id(value: &str) -> bool {
    value.len() == 32
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || matches!(b, b'a'..=b'f'))
}
pub(crate) fn origin(address: &str) -> Result<String> {
    let value = if address.starts_with("https://") {
        address.to_string()
    } else {
        format!("https://{address}")
    };
    let mut url = url::Url::parse(&value)
        .context(ErrorCode::InvalidAddress.error("invalid relay address"))?;
    if url.scheme() != "https"
        || !url.host_str().is_some_and(|host| {
            host.bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-'))
        })
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || !matches!(url.path(), "" | "/")
    {
        bail!(
            ErrorCode::InvalidAddress.error("use the relay host and optional port, without a path")
        );
    }
    if !url.host_str().is_some_and(|host| {
        host.as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphanumeric)
    }) {
        bail!(ErrorCode::InvalidAddress.error("invalid relay host"));
    }
    url.set_path("");
    Ok(url.to_string().trim_end_matches('/').to_string())
}
pub(crate) fn relay_origin(address: &str) -> Result<String> {
    let mut url = url::Url::parse(address)?;
    url.set_path("");
    origin(url.as_str())
}
pub(crate) fn short_link(link: &str) -> Option<Result<(String, String)>> {
    let rest = link.strip_prefix("xrun://")?;
    let (address, code) = rest.split_once('#')?;
    if address.trim_end_matches('/').contains('/') {
        return None;
    }
    Some(origin(address).and_then(|address| Ok((address, normalize(code)?))))
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Hello {
    pub(crate) invitation_id: String,
    pub(crate) origin: String,
    pub(crate) csr: String,
    pub(crate) message: String,
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "pairing", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum Frame {
    Hello(Hello),
    Challenge { message: String },
    Confirm { mac: String },
    Welcome { mac: String },
    Encrypted { body: String },
    Error { code: String, message: String },
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn codes_and_addresses_have_one_canonical_form() -> Result<()> {
        assert_eq!(normalize(" k7m4-p9q2 ")?, "K7M4P9Q2");
        assert!(normalize("K7M4-P9O2").is_err());
        assert!(normalize("K7M4-P9Q").is_err());
        assert_eq!(origin("relay.example:9528")?, "https://relay.example:9528");
        assert!(origin("relay.example/private").is_err());
        assert!(origin("http://relay.example").is_err());
        assert!(origin("https://user:pass@relay.example").is_err());
        assert_eq!(
            short_link("xrun://relay.example#K7M4-P9Q2").unwrap()?.1,
            "K7M4P9Q2"
        );
        assert!(short_link("xrun://relay.example/private/net/root/pin#secret").is_none());
        assert_eq!(random_chars(6).len(), 6);
        Ok(())
    }
}
