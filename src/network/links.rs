//! Invitation and relay-link validation, including transport trust policy.
use crate::error::ErrorCode;
use crate::{crypto, membership::SignedRoster};
use anyhow::{Context, Result, bail};

pub fn invitation_link(roster: &SignedRoster, token: &str) -> Result<String> {
    let r = &roster.roster;
    let addresses = r
        .relay_addresses
        .iter()
        .map(|s| {
            s.strip_prefix("https://")
                .context(ErrorCode::InvalidRelay.error("HTTPS required"))
        })
        .collect::<Result<Vec<_>>>()?
        .join(",");
    Ok(format!(
        "xrun://{addresses}/{}/{}/{}/{}#{token}",
        r.network_id,
        r.manager_id,
        crypto::ca_spki_pin(&roster.ca_pem)?,
        relay_pin(&r.relay_ca_pem)?
    ))
}
pub(super) struct Invitation {
    pub(super) addresses: Vec<String>,
    pub(super) network: String,
    pub(super) manager: String,
    pub(super) root_pin: String,
    pub(super) relay_pin: String,
    pub(super) token: String,
}
pub(super) fn parse_link(link: &str) -> Result<Invitation> {
    let rest = link
        .strip_prefix("xrun://")
        .context(ErrorCode::InvalidLink.error("expected xrun://"))?;
    let (rest, token) = rest
        .split_once('#')
        .context(ErrorCode::InvalidLink.error("missing token"))?;
    let mut parts: Vec<_> = rest.rsplitn(5, '/').collect();
    parts.reverse();
    if parts.len() != 5 {
        bail!(ErrorCode::InvalidLink.error("expected an end-to-end network invitation"))
    }
    let pin_valid = |s: &str, n| {
        s.len() == n
            && s.bytes()
                .all(|b| b.is_ascii_lowercase() || matches!(b, b'2'..=b'7'))
    };
    if !pin_valid(parts[3], 52)
        || !(parts[4] == "webpki" || pin_valid(parts[4], 52))
        || !pin_valid(token, 26)
        || parts[1] != format!("net_{}", parts[3])
    {
        bail!(ErrorCode::InvalidLink.error("malformed network fingerprint or token"))
    }
    crate::membership::device_name(parts[2])?;
    let addresses = parts[0]
        .split(',')
        .map(|a| {
            if parts[4] == "webpki" {
                return Ok(endpoint(&format!("https://{a}"))?.addresses.remove(0));
            }
            let (host, route) = a
                .split_once('/')
                .context(ErrorCode::InvalidLink.error("missing relay route"))?;
            crate::client::validate_address(host)?;
            if !crate::relay::valid_route(route) {
                bail!(ErrorCode::InvalidLink.error("invalid relay route"))
            }
            Ok(format!("https://{a}"))
        })
        .collect::<Result<Vec<_>>>()?;
    if addresses.is_empty() || addresses.len() > 8 {
        bail!(ErrorCode::InvalidLink.error("invalid relay address count"))
    }
    Ok(Invitation {
        addresses,
        network: parts[1].into(),
        manager: parts[2].into(),
        root_pin: parts[3].into(),
        relay_pin: parts[4].into(),
        token: token.into(),
    })
}
pub(super) struct RelayEndpoint {
    pub(super) addresses: Vec<String>,
    pub(super) pin: String,
}
pub(super) fn relay_pin(ca: &str) -> Result<String> {
    if ca.is_empty() {
        Ok("webpki".into())
    } else {
        crypto::ca_spki_pin(ca)
    }
}
pub(super) async fn discover_relay_ca(address: &str, pin: &str) -> Result<String> {
    if pin == "webpki" {
        Ok(String::new())
    } else {
        crypto::discover_ca(address, pin).await
    }
}
pub(super) fn endpoint(link: &str) -> Result<RelayEndpoint> {
    if link.starts_with("https://") {
        let url = url::Url::parse(link)?;
        if url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || !crate::relay::valid_route(url.path().trim_start_matches('/'))
            || url.path().matches('/').count() != 1
        {
            bail!(
                ErrorCode::InvalidRelay
                    .error("expected a complete HTTPS relay address with its random route")
            )
        }
        return Ok(RelayEndpoint {
            addresses: vec![url.into()],
            pin: "webpki".into(),
        });
    }
    let value =
        link.strip_prefix("xrun-relay://")
            .context(ErrorCode::InvalidRelay.error(
                "use the HTTPS address or deployment link printed by the relay deployment",
            ))?;
    let (value, route) = value
        .split_once('#')
        .context(ErrorCode::InvalidRelay.error("missing relay route"))?;
    let (addresses, pin) = value
        .split_once('/')
        .context(ErrorCode::InvalidRelay.error("missing transport fingerprint"))?;
    let valid = |s: &str, n| {
        s.len() == n
            && s.bytes()
                .all(|b| b.is_ascii_lowercase() || matches!(b, b'2'..=b'7'))
    };
    if !valid(pin, 52) || !crate::relay::valid_route(route) {
        bail!(ErrorCode::InvalidRelay.error("malformed fingerprint or relay route"))
    }
    let addresses = addresses
        .split(',')
        .map(|a| {
            crate::client::validate_address(a)?;
            Ok(format!("https://{a}/{route}"))
        })
        .collect::<Result<Vec<_>>>()?;
    if addresses.is_empty() || addresses.len() > 8 {
        bail!(ErrorCode::InvalidRelay.error("invalid address count"))
    }
    Ok(RelayEndpoint {
        addresses,
        pin: pin.into(),
    })
}
