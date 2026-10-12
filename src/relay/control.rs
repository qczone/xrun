//! Control ownership, heartbeat and generation replacement.
use super::*;
pub(super) async fn status_route(
    State(app): State<Arc<App>>,
    Path(network): Path<String>,
    Extension(permit): Extension<TransportPermit>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> ApiResult<Response> {
    let selected = protocol(&headers)?;
    if network != "probe" {
        app.admission.check(&network, false)?;
    }
    Ok(negotiated(
        ws.max_message_size(MAX_MESSAGE)
            .max_frame_size(MAX_MESSAGE)
            .on_upgrade(move |mut ws| async move {
                let result = async {
                    if network == "probe" {
                        send(
                            &mut ws,
                            &RelayMessage::Challenge {
                                nonce: crypto::random_token(),
                                authority: app.admission.authority(),
                            },
                        )
                        .await?;
                        send(
                            &mut ws,
                            &RelayMessage::RelayInfo {
                                owner: app.admission.owner(),
                            },
                        )
                        .await?;
                        return Ok(());
                    }
                    let path = format!("/networks/{network}/status");
                    let (_, manager) = member(&app, &mut ws, &network, &path, &permit).await?;
                    let devices = app
                        .connections
                        .lock()
                        .await
                        .controls
                        .keys()
                        .filter(|(n, _)| n == &network)
                        .map(|(_, id)| id.clone())
                        .collect();
                    let pending_authority = if manager {
                        app.admission.pending()?
                    } else {
                        Vec::new()
                    };
                    send(
                        &mut ws,
                        &RelayMessage::Status {
                            devices,
                            pending_authority,
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
pub(super) async fn control_route(
    State(app): State<Arc<App>>,
    Path(network): Path<String>,
    Extension(permit): Extension<TransportPermit>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> ApiResult<Response> {
    let selected = protocol(&headers)?;
    app.admission.check(&network, true)?;
    Ok(negotiated(
        ws.max_message_size(MAX_MESSAGE)
            .max_frame_size(MAX_MESSAGE)
            .on_upgrade(move |mut ws| async move {
                let result = control(&app, &network, &mut ws, &permit, selected).await;
                finish(&mut ws, result).await;
            }),
        selected,
    ))
}
async fn control(
    app: &App,
    network: &str,
    ws: &mut WebSocket,
    permit: &TransportPermit,
    protocol: u32,
) -> Result<()> {
    // Only the holder of a device key can take over that device's routing slot.
    // Authorization still happens inside the end-to-end tunnel.
    let path = format!("/networks/{network}/control");
    let (id, manager) = member(app, ws, network, &path, permit).await?;
    // An existing manager can establish the same binding when upgrading a relay.
    if app.admission.owner().is_none() && manager {
        app.admission.claim(network, &id, manager)?;
    }
    app.admission.check(network, false)?;
    let manager = app.admission.manager(&id, manager)?;
    let key = (network.to_owned(), id.clone());
    let generation = uuid::Uuid::new_v4().simple().to_string();
    let (tx, mut rx) = mpsc::channel(16);
    let (cancel, mut closed) = watch::channel(false);
    {
        let mut connections = app.connections.lock().await;
        if !connections.controls.contains_key(&key)
            && (connections.controls.len() >= MAX_CONTROL_CONNECTIONS
                || connections
                    .controls
                    .keys()
                    .filter(|(n, _)| n == network)
                    .count()
                    >= MAX_NETWORK_CONTROLS)
        {
            bail!(ErrorCode::ConnectionLimit.error("too many control connections"))
        }
        if let Some(old) = connections.controls.insert(
            key.clone(),
            ControlConnection {
                authority_generation: app
                    .admission
                    .authority()
                    .expect("authenticated authority")
                    .roster
                    .roster
                    .authority_generation,
                protocol,
                generation: generation.clone(),
                manager,
                tx,
                cancel,
            },
        ) {
            let _ = old.cancel.send(true);
        }
        for session in connections.sessions.values() {
            if session.network == network && session.target == id {
                let _ = session.cancel.send(true);
            }
        }
    }
    let result = async {
        send(ws, &RelayMessage::HelloAck { generation: generation.clone() }).await?;
        let mut tick = tokio::time::interval(HEARTBEAT_INTERVAL);
        let mut delivery = tokio::time::interval(std::time::Duration::from_secs(5));
        delivery.tick().await;
        let mut last = Instant::now();
        loop {
            tokio::select! {
                _ = closed.changed() => return Ok(()),
                _ = tick.tick() => control_heartbeat(ws, last).await?,
                _ = delivery.tick() => {
                    if app.admission.pending()?.contains(&id) && let Some(state) = app.admission.authority() {
                        send(ws, &RelayMessage::Authority {state}).await?;
                    }
                },
                message = rx.recv() => send(ws, &message.context(ErrorCode::ConnectionClosed.error("control sender stopped"))?).await?,
                message = ws.next() => control_message(app, ws, message, network, &id, &generation, &mut last).await?,
            }
        }
    }.await;
    let mut connections = app.connections.lock().await;
    if connections
        .controls
        .get(&key)
        .is_some_and(|v| v.generation == generation)
    {
        connections.controls.remove(&key);
        for session in connections.sessions.values() {
            if session.network == network && session.target == id {
                let _ = session.cancel.send(true);
            }
        }
    }
    result
}
async fn control_heartbeat(ws: &mut WebSocket, last: Instant) -> Result<()> {
    if last.elapsed() > HEARTBEAT_TIMEOUT {
        bail!(ErrorCode::ControlTimeout.error("endpoint stopped responding"));
    }
    tokio::time::timeout(CONNECT_TIMEOUT, ws.send(Message::Ping(vec![].into()))).await??;
    Ok(())
}
async fn control_message(
    app: &App,
    ws: &mut WebSocket,
    message: Option<std::result::Result<Message, axum::Error>>,
    network: &str,
    device: &str,
    generation: &str,
    last: &mut Instant,
) -> Result<()> {
    match message.context(ErrorCode::ConnectionClosed.error("control socket closed"))?? {
        Message::Ping(bytes) => {
            *last = Instant::now();
            ws.send(Message::Pong(bytes)).await?;
        }
        Message::Pong(_) => *last = Instant::now(),
        Message::Text(text) => {
            *last = Instant::now();
            let message = serde_json::from_str::<RelayMessage>(&text)?;
            if let RelayMessage::Authority { state } = &message {
                let control = app
                    .connections
                    .lock()
                    .await
                    .controls
                    .get(&(network.into(), device.into()))
                    .map(|control| (control.manager, control.generation.clone()));
                if control != Some((true, generation.into()))
                    || state.roster.roster.manager_id != device
                {
                    bail!(
                        ErrorCode::NotManager
                            .error("only the current manager control can publish authority")
                    );
                }
                app.admission.publish(state)?;
                super::publish_connections(app, state).await;
                send(
                    ws,
                    &RelayMessage::AuthorityAck {
                        authority_generation: state.roster.roster.authority_generation,
                        version: state.roster.roster.version,
                        hash: state.roster.hash()?,
                    },
                )
                .await?;
                return Ok(());
            }
            if let RelayMessage::AuthorityAck {
                authority_generation,
                version,
                hash,
            } = &message
            {
                app.admission
                    .acknowledge(device, *authority_generation, *version, hash)?;
                return Ok(());
            }
            if pairing::control(app, ws, network, device, generation, &message).await? {
                return Ok(());
            }
            let RelayMessage::Reject { session_id, error } = message else {
                bail!(ErrorCode::InvalidMessage.error("unexpected control message"));
            };
            let mut connections = app.connections.lock().await;
            if let Some(session) = connections.sessions.get_mut(&session_id)
                && session.network == network
                && session.target == device
                && session.generation == generation
                && let Some(sender) = session.claim.take()
            {
                let _ = sender.send(Err(*error));
            }
        }
        _ => bail!(ErrorCode::ConnectionClosed.error("control socket closed")),
    }
    Ok(())
}
