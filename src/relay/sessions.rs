//! Session admission, generation-bound handoff and ciphertext forwarding.
use super::*;
async fn claimed_session(
    receiver: oneshot::Receiver<std::result::Result<WebSocket, Data>>,
) -> Result<WebSocket> {
    match tokio::time::timeout(CONNECT_TIMEOUT, receiver).await?? {
        Ok(socket) => Ok(socket),
        Err(Data::Error { code, message }) => {
            bail!(crate::error::CodedError::from_wire(code, message))
        }
        _ => bail!(ErrorCode::InvalidMessage.error("invalid session rejection")),
    }
}
pub(super) async fn source_route(
    State(app): State<Arc<App>>,
    Path((network, target)): Path<(String, String)>,
    Extension(permit): Extension<TransportPermit>,
    Extension(peer): Extension<std::net::SocketAddr>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> ApiResult<Response> {
    version(&headers)?;
    Ok(ws
        .max_message_size(MAX_MESSAGE)
        .max_frame_size(MAX_MESSAGE)
        .on_upgrade(move |mut ws| async move {
            let result = source(&app, &network, &target, peer.ip(), &mut ws, &permit).await;
            finish(&mut ws, result).await;
        }))
}
async fn source(
    app: &App,
    network: &str,
    target: &str,
    source: std::net::IpAddr,
    ws: &mut WebSocket,
    permit: &TransportPermit,
) -> Result<()> {
    let path = format!("/networks/{network}/connect/{target}");
    let anonymous = authenticate(ws, network, &path, permit).await?.is_none();
    let sid = uuid::Uuid::new_v4().simple().to_string();
    let (tx, rx) = oneshot::channel();
    let (cancel, mut closed) = watch::channel(false);
    {
        let mut connections = app.connections.lock().await;
        let to_target = |session: &&Session| session.network == network && session.target == target;
        if connections.sessions.len() >= MAX_RELAY_SESSIONS
            || connections.sessions.values().filter(to_target).count() >= MAX_TARGET_SESSIONS
            || connections
                .sessions
                .values()
                .filter(|session| session.source == source)
                .count()
                >= MAX_SOURCE_SESSIONS
            || (anonymous
                && connections
                    .sessions
                    .values()
                    .filter(to_target)
                    .filter(|session| session.anonymous)
                    .count()
                    >= MAX_ANONYMOUS_TARGET_SESSIONS)
        {
            bail!(ErrorCode::SessionLimit.error("too many concurrent relay sessions"))
        }
        let control = connections
            .controls
            .get(&(network.into(), target.into()))
            .context(ErrorCode::DeviceOffline.error("target is offline"))?;
        // Joining and renewal clients have no usable member certificate; they
        // may only reach the device that proved it holds the network root key.
        if anonymous && !control.manager {
            bail!(ErrorCode::Unauthenticated.error("member proof required"))
        }
        let generation = control.generation.clone();
        control
            .tx
            .try_send(RelayMessage::Incoming {
                session_id: sid.clone(),
            })
            .context(ErrorCode::DeviceBusy.error("control queue is full"))?;
        connections.sessions.insert(
            sid.clone(),
            Session {
                network: network.into(),
                source,
                anonymous,
                target: target.into(),
                generation,
                claim: Some(tx),
                cancel,
            },
        );
    }
    let result = async {
        let mut target = tokio::select! {
            value = claimed_session(rx) => value?,
            _ = closed.changed() => bail!(ErrorCode::ConnectionClosed.error("session was cancelled")),
            _ = ws.next() => bail!(ErrorCode::ConnectionClosed.error("source disconnected before session establishment")),
        };
        send(ws, &RelayMessage::Connected { flow_control: false }).await?;
        let result = bridge(ws, &mut target, &mut closed).await;
        let _ = tokio::time::timeout(CLOSE_TIMEOUT, target.close()).await;
        result
    }.await;
    app.connections.lock().await.sessions.remove(&sid);
    result
}
pub(super) async fn attach_route(
    State(app): State<Arc<App>>,
    Path((network, target, generation, sid)): Path<(String, String, String, String)>,
    Extension(permit): Extension<TransportPermit>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> ApiResult<Response> {
    version(&headers)?;
    Ok(ws
        .max_message_size(MAX_MESSAGE)
        .max_frame_size(MAX_MESSAGE)
        .on_upgrade(move |mut ws| async move {
            let result = async {
                // The generation and session ID reach only the authenticated
                // control connection, so a matching attach needs no new proof.
                let sender = {
                    let mut connections = app.connections.lock().await;
                    let control = connections
                        .controls
                        .get(&(network.clone(), target.clone()))
                        .context(ErrorCode::InvalidSession.error("target control is missing"))?;
                    if control.generation != generation {
                        bail!(ErrorCode::InvalidSession.error("control binding mismatch"))
                    }
                    let session = connections.sessions.get_mut(&sid).context(
                        ErrorCode::InvalidSession.error("session is missing or expired"),
                    )?;
                    if session.network != network
                        || session.target != target
                        || session.generation != generation
                        || *session.cancel.borrow()
                    {
                        bail!(ErrorCode::InvalidSession.error("session binding mismatch"))
                    }
                    session
                        .claim
                        .take()
                        .context(ErrorCode::InvalidSession.error("session was already claimed"))?
                };
                permit.authenticated();
                send(
                    &mut ws,
                    &RelayMessage::Connected {
                        flow_control: false,
                    },
                )
                .await?;
                sender.send(Ok(ws)).map_err(|_| {
                    anyhow::anyhow!(ErrorCode::ConnectionClosed.error("source disappeared"))
                })?;
                Ok::<_, anyhow::Error>(())
            }
            .await;
            if let Err(error) = result {
                tracing::debug!(%error, "target attachment failed");
            }
        }))
}
async fn bridge(
    source: &mut WebSocket,
    target: &mut WebSocket,
    closed: &mut watch::Receiver<bool>,
) -> Result<()> {
    let (source_tx, source_rx) = source.split();
    let (target_tx, target_rx) = target.split();
    tokio::select! {
        result=direction(source_rx,target_tx)=>result,
        result=direction(target_rx,source_tx)=>result,
        _=closed.changed()=>Ok(()),
    }
}
async fn direction<R, W>(mut reader: R, mut writer: W) -> Result<()>
where
    R: futures_util::Stream<Item = std::result::Result<Message, axum::Error>> + Unpin,
    W: futures_util::Sink<Message, Error = axum::Error> + Unpin,
{
    loop {
        let message = tokio::time::timeout(RELAY_IDLE_TIMEOUT, reader.next())
            .await?
            .context(ErrorCode::ConnectionClosed.error("ciphertext stream ended"))??;
        match &message {
            Message::Binary(bytes) if bytes.len() <= FILE_CHUNK => {}
            Message::Ping(_) | Message::Pong(_) => {}
            Message::Close(_) => return Ok(()),
            _ => {
                bail!(ErrorCode::InvalidMessage.error("relay only accepts encrypted binary frames"))
            }
        }
        tokio::time::timeout(RELAY_IDLE_TIMEOUT, writer.send(message)).await??;
    }
}
