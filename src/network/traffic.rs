//! Authenticated relay usage queries; only a root-key proof grants network scope.
use super::{
    authority, current, manager,
    transport::{authenticate, open_via, receive},
};
use crate::{
    config::Identity,
    error::ErrorCode,
    net,
    protocol::{AUTH_TIMEOUT, RelayMessage, TrafficQuery, TrafficReport},
};
use anyhow::{Result, bail};

pub(crate) async fn traffic(id: &Identity, query: TrafficQuery) -> Result<TrafficReport> {
    query.validate()?;
    let roster = current(id)?;
    let authority = authority(id)?;
    let root = if authority.manager_id == id.device_id {
        Some(manager(id)?)
    } else {
        None
    };
    let path = format!("/networks/{}/traffic", authority.network_id);
    let (mut ws, _) = open_via(id, &roster, &path).await?;
    let result = tokio::time::timeout(AUTH_TIMEOUT, async {
        authenticate(
            &mut ws,
            Some(id),
            &authority.network_id,
            &path,
            root.as_ref(),
        )
        .await?;
        if !matches!(receive(&mut ws).await?, RelayMessage::TrafficReady) {
            bail!(
                ErrorCode::InvalidMessage.error("expected traffic authentication acknowledgement")
            );
        }
        net::send(
            &mut ws,
            &RelayMessage::TrafficQuery {
                query: query.clone(),
            },
        )
        .await?;
        let RelayMessage::Traffic { report } = receive(&mut ws).await? else {
            bail!(ErrorCode::InvalidMessage.error("expected relay traffic"));
        };
        let expected_scope = root.is_none().then_some(id.device_id.as_str());
        if report.network_id != authority.network_id
            || report.device_id.as_deref() != expected_scope
            || report.period != query.period
            || report.devices.len() > query.limit as usize
            || report.daily.len() > 31
        {
            bail!(ErrorCode::InvalidMessage.error("relay traffic scope or period differs"));
        }
        Ok(report)
    })
    .await;
    net::close(&mut ws).await;
    result.map_err(|_| {
        anyhow::anyhow!(ErrorCode::ConnectTimeout.error("relay traffic query timed out"))
    })?
}
