//! Restricted anonymous rendezvous; registration requires the proven owner control.
use super::*;
#[derive(Clone)]
pub(super) struct Room {
    pub(super) id: String,
    pub(super) network: String,
    pub(super) manager: String,
    pub(super) generation: String,
    pub(super) expires_at: i64,
}
pub(super) async fn control(
    app: &App,
    ws: &mut WebSocket,
    network: &str,
    device: &str,
    generation: &str,
    message: &RelayMessage,
) -> Result<bool> {
    let (id, registration) = match message {
        RelayMessage::PairingRegister {
            invitation_id,
            locator,
            expires_at,
        } => (invitation_id, Some((locator, expires_at))),
        RelayMessage::PairingCancel { invitation_id } => (invitation_id, None),
        _ => return Ok(false),
    };
    let result = async {
        let mut connections = app.connections.lock().await;
        let current = connections
            .controls
            .get(&(network.into(), device.into()))
            .context(ErrorCode::DeviceOffline.error("manager control is missing"))?;
        if !current.manager || current.generation != generation {
            bail!(ErrorCode::NotManager.error("pairing registration requires the current manager"));
        }
        ProtocolRange::require(current.protocol, 4)?;
        if !crate::pairing::valid_id(id) {
            bail!(ErrorCode::InvalidRequest.error("invalid invitation identity"));
        }
        connections
            .pairings
            .retain(|_, room| room.expires_at > now_ms());
        let Some((locator, expires_at)) = registration else {
            connections.pairings.retain(|_, room| &room.id != id);
            return Ok(());
        };
        if !crate::pairing::valid_locator(locator)
            || *expires_at <= now_ms()
            || *expires_at > now_ms() + crate::pairing::LIFETIME_MS + 30_000
        {
            bail!(ErrorCode::InvalidRequest.error("invalid pairing locator or expiration"));
        }
        if let Some(room) = connections.pairings.get(locator)
            && &room.id != id
        {
            bail!(ErrorCode::PairingBusy.error("pairing locator already allocated"));
        }
        if connections.pairings.len() >= 256 && !connections.pairings.contains_key(locator) {
            bail!(ErrorCode::InvitationLimit.error("too many active pairing sessions"));
        }
        connections.pairings.insert(
            locator.clone(),
            Room {
                id: id.clone(),
                network: network.into(),
                manager: device.into(),
                generation: generation.into(),
                expires_at: *expires_at,
            },
        );
        Ok(())
    }
    .await;
    send(
        ws,
        &RelayMessage::PairingRegistered {
            invitation_id: id.clone(),
            error: result
                .err()
                .map(|error: anyhow::Error| Box::new(Data::error(&error))),
        },
    )
    .await?;
    Ok(true)
}
pub(super) async fn route(
    State(app): State<Arc<App>>,
    Path(locator): Path<String>,
    Extension(permit): Extension<TransportPermit>,
    Extension(peer): Extension<std::net::SocketAddr>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> ApiResult<Response> {
    let selected = protocol(&headers)?;
    ProtocolRange::require(selected, 4)?;
    if !crate::pairing::valid_locator(&locator) {
        return Err(ErrorCode::InvalidCode
            .error("invalid pairing locator")
            .into());
    }
    let room = app
        .connections
        .lock()
        .await
        .pairings
        .get(&locator)
        .filter(|room| room.expires_at > now_ms())
        .cloned()
        .context(ErrorCode::InvalidCode.error("pairing code is unavailable or expired"))?;
    app.admission.check(&room.network, false)?;
    Ok(negotiated(
        ws.max_message_size(crate::pairing::MAX_FRAME)
            .max_frame_size(crate::pairing::MAX_FRAME)
            .on_upgrade(move |mut ws| async move {
                let result = async {
                    send(
                        &mut ws,
                        &RelayMessage::PairingInfo {
                            invitation_id: room.id.clone(),
                            locator,
                        },
                    )
                    .await?;
                    sessions::source(
                        &app,
                        &room.network,
                        &room.manager,
                        peer.ip(),
                        &mut ws,
                        &permit,
                        sessions::SourceOptions {
                            protocol: selected,
                            pairing: Some(&room),
                        },
                    )
                    .await
                }
                .await;
                finish(&mut ws, result).await;
            }),
        selected,
    ))
}
pub(super) async fn bridge(
    app: &App,
    network: &str,
    manager: &str,
    source: &mut WebSocket,
    target: &mut WebSocket,
    closed: &mut watch::Receiver<bool>,
) -> Result<()> {
    let deadline = tokio::time::sleep(std::time::Duration::from_secs(20));
    tokio::pin!(deadline);
    let mut transferred = 0;
    loop {
        let (message, from_source) = tokio::select! {
            message = source.next() => (message, true),
            message = target.next() => (message, false),
            _ = &mut deadline => bail!(ErrorCode::PairingTimeout.error("pairing tunnel timed out")),
            _ = closed.changed() => return Ok(()),
        };
        let message =
            message.context(ErrorCode::ConnectionClosed.error("pairing tunnel closed"))??;
        match &message {
            Message::Binary(bytes) => {
                transferred += bytes.len();
                if bytes.len() > crate::pairing::MAX_FRAME || transferred > 64 * 1024 {
                    bail!(ErrorCode::MessageTooLarge.error("pairing tunnel exceeds budget"));
                }
                app.traffic.record(
                    network,
                    if from_source { None } else { Some(manager) },
                    bytes.len(),
                    false,
                );
                app.traffic.record(
                    network,
                    if from_source { Some(manager) } else { None },
                    bytes.len(),
                    true,
                );
            }
            Message::Close(_) => return Ok(()),
            Message::Ping(_) | Message::Pong(_) => {}
            _ => bail!(ErrorCode::InvalidMessage.error("pairing only accepts binary records")),
        }
        let writer = if from_source {
            &mut *target
        } else {
            &mut *source
        };
        tokio::select! {
            result = writer.send(message) => result?,
            _ = &mut deadline => bail!(ErrorCode::PairingTimeout.error("pairing tunnel timed out")),
            _ = closed.changed() => return Ok(()),
        }
    }
}
