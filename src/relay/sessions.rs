//! Session admission, generation-bound handoff and ciphertext forwarding.
use super::*;

impl Connections {
    fn reserve_session(
        &mut self,
        network: &str,
        target: &str,
        source: std::net::IpAddr,
        device: Option<&str>,
    ) -> Result<()> {
        let to_target = |session: &&Session| session.network == network && session.target == target;
        let from_source = |session: &&Session| match device {
            Some(device) => session.network == network && session.device.as_deref() == Some(device),
            None => session.anonymous && session.source == source,
        };
        loop {
            let target_full =
                self.sessions.values().filter(to_target).count() >= MAX_TARGET_SESSIONS;
            let source_full =
                self.sessions.values().filter(from_source).count() >= MAX_SOURCE_SESSIONS;
            let total_full = self.sessions.len() >= MAX_RELAY_SESSIONS;
            let anonymous_full = device.is_none()
                && self
                    .sessions
                    .values()
                    .filter(to_target)
                    .filter(|session| session.anonymous)
                    .count()
                    >= MAX_ANONYMOUS_TARGET_SESSIONS;
            if anonymous_full {
                bail!(ErrorCode::SessionLimit.error("too many anonymous relay sessions"));
            }
            if !target_full && !source_full && !total_full {
                return Ok(());
            }
            let oldest = self
                .sessions
                .iter()
                .filter(|(_, session)| {
                    if source_full {
                        from_source(session)
                    } else if target_full {
                        to_target(session)
                    } else {
                        true
                    }
                })
                .filter_map(|(sid, session)| session.cached_at.map(|since| (sid.clone(), since)))
                .min_by_key(|(_, since)| *since)
                .map(|(sid, _)| sid);
            let Some(sid) = oldest else {
                bail!(ErrorCode::SessionLimit.error("too many active relay sessions"));
            };
            if let Some(session) = self.sessions.remove(&sid) {
                let _ = session.cancel.send(true);
            }
        }
    }
}
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
    let selected = protocol(&headers)?;
    Ok(negotiated(
        ws.max_message_size(MAX_MESSAGE)
            .max_frame_size(MAX_MESSAGE)
            .on_upgrade(move |mut ws| async move {
                let result = source(
                    &app,
                    &network,
                    &target,
                    peer.ip(),
                    &mut ws,
                    &permit,
                    selected,
                )
                .await;
                finish(&mut ws, result).await;
            }),
        selected,
    ))
}
async fn source(
    app: &App,
    network: &str,
    target: &str,
    source: std::net::IpAddr,
    ws: &mut WebSocket,
    permit: &TransportPermit,
    protocol: u32,
) -> Result<()> {
    let path = format!("/networks/{network}/connect/{target}");
    let device = authenticate(ws, network, &path, permit)
        .await?
        .map(|(device, _)| device);
    let anonymous = device.is_none();
    let sid = uuid::Uuid::new_v4().simple().to_string();
    let (tx, rx) = oneshot::channel();
    let (cancel, mut closed) = watch::channel(false);
    {
        let mut connections = app.connections.lock().await;
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
        let target_control = control.tx.clone();
        connections.reserve_session(network, target, source, device.as_deref())?;
        target_control
            .try_send(RelayMessage::Incoming {
                session_id: sid.clone(),
            })
            .context(ErrorCode::DeviceBusy.error("control queue is full"))?;
        connections.sessions.insert(
            sid.clone(),
            Session {
                network: network.into(),
                source,
                device,
                cached_at: None,
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
        let result = bridge(app, &sid, ws, &mut target, &mut closed, !anonymous && protocol >= 2).await;
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
    let selected = protocol(&headers)?;
    Ok(negotiated(
        ws.max_message_size(MAX_MESSAGE)
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
                            .context(
                                ErrorCode::InvalidSession.error("target control is missing"),
                            )?;
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
                        session.claim.take().context(
                            ErrorCode::InvalidSession.error("session was already claimed"),
                        )?
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
            }),
        selected,
    ))
}
async fn bridge(
    app: &App,
    sid: &str,
    source: &mut WebSocket,
    target: &mut WebSocket,
    closed: &mut watch::Receiver<bool>,
    cacheable: bool,
) -> Result<()> {
    let (source_tx, source_rx) = source.split();
    let (target_tx, target_rx) = target.split();
    let source_tx = Mutex::new(source_tx);
    let target_tx = Mutex::new(target_tx);
    let activity = Activity::new(tokio::time::Instant::now());
    tokio::select! {
        result = source_direction(app, sid, cacheable, source_rx, &target_tx, &source_tx, &activity) => result,
        result = direction(target_rx, &source_tx, &activity) => result,
        result = idle_session(&activity) => result,
        _ = closed.changed() => Ok(()),
    }
}
type Activity = std::sync::Mutex<tokio::time::Instant>;
async fn idle_session(activity: &Activity) -> Result<()> {
    loop {
        let deadline = *activity.lock().unwrap() + RELAY_IDLE_TIMEOUT;
        tokio::time::sleep_until(deadline).await;
        // Traffic moves the deadline without waking another task for every frame.
        if activity.lock().unwrap().elapsed() >= RELAY_IDLE_TIMEOUT {
            bail!(ErrorCode::ConnectTimeout.error("relay session was idle too long"));
        }
    }
}
async fn source_direction<R, W, S>(
    app: &App,
    sid: &str,
    cacheable: bool,
    mut reader: R,
    target: &Mutex<W>,
    source: &Mutex<S>,
    activity: &Activity,
) -> Result<()>
where
    R: futures_util::Stream<Item = std::result::Result<Message, axum::Error>> + Unpin,
    W: futures_util::Sink<Message, Error = axum::Error> + Unpin,
    S: futures_util::Sink<Message, Error = axum::Error> + Unpin,
{
    let mut idle = false;
    loop {
        let message = reader
            .next()
            .await
            .context(ErrorCode::ConnectionClosed.error("source ciphertext stream ended"))??;
        *activity.lock().unwrap() = tokio::time::Instant::now();
        match &message {
            Message::Text(text) if cacheable && text.len() <= CACHE_STATE_MESSAGE_BYTES => {
                let RelayMessage::CacheState { idle: next } = serde_json::from_str(text)? else {
                    bail!(ErrorCode::InvalidMessage.error("expected relay cache state"));
                };
                {
                    let mut connections = app.connections.lock().await;
                    let session = connections.sessions.get_mut(sid).context(
                        ErrorCode::SessionUnavailable.error("cached relay session was reclaimed"),
                    )?;
                    session.cached_at = next.then(Instant::now);
                }
                idle = next;
                forward(
                    source,
                    Message::Text(
                        serde_json::to_string(&RelayMessage::CacheState { idle })?.into(),
                    ),
                )
                .await?;
                continue;
            }
            Message::Binary(bytes) if !idle && bytes.len() <= FILE_CHUNK => {}
            Message::Ping(_) | Message::Pong(_) => {}
            Message::Close(_) => return Ok(()),
            _ => bail!(
                ErrorCode::InvalidMessage
                    .error("reactivate a cached tunnel before sending ciphertext")
            ),
        }
        forward(target, message).await?;
    }
}
async fn direction<R, W>(mut reader: R, writer: &Mutex<W>, activity: &Activity) -> Result<()>
where
    R: futures_util::Stream<Item = std::result::Result<Message, axum::Error>> + Unpin,
    W: futures_util::Sink<Message, Error = axum::Error> + Unpin,
{
    loop {
        let message = reader
            .next()
            .await
            .context(ErrorCode::ConnectionClosed.error("ciphertext stream ended"))??;
        *activity.lock().unwrap() = tokio::time::Instant::now();
        match &message {
            Message::Binary(bytes) if bytes.len() <= FILE_CHUNK => {}
            Message::Ping(_) | Message::Pong(_) => {}
            Message::Close(_) => return Ok(()),
            _ => {
                bail!(ErrorCode::InvalidMessage.error("relay only accepts encrypted binary frames"))
            }
        }
        forward(writer, message).await?;
    }
}
async fn forward<W>(writer: &Mutex<W>, message: Message) -> Result<()>
where
    W: futures_util::Sink<Message, Error = axum::Error> + Unpin,
{
    tokio::time::timeout(RELAY_IDLE_TIMEOUT, async {
        writer.lock().await.send(message).await
    })
    .await??;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn one_way_activity_keeps_the_session_alive_but_silence_expires_it() {
        let (source, mut from_source) = mpsc::unbounded_channel();
        let (target, mut from_target) = mpsc::unbounded_channel();
        let idle = tokio::spawn(async move {
            let app = App {
                connections: Mutex::new(Connections::default()),
            };
            let activity = Activity::new(tokio::time::Instant::now());
            let source = futures_util::stream::poll_fn(move |cx| from_source.poll_recv(cx));
            let target = futures_util::stream::poll_fn(move |cx| from_target.poll_recv(cx));
            let sink = || {
                Mutex::new(Box::pin(futures_util::sink::unfold(
                    (),
                    |(), _: Message| async { Ok::<_, axum::Error>(()) },
                )))
            };
            let source_tx = sink();
            let target_tx = sink();
            tokio::select! {
                result = source_direction(&app, "session", false, source, &target_tx, &source_tx, &activity) => result,
                result = direction(target, &source_tx, &activity) => result,
                result = idle_session(&activity) => result,
            }
        });
        tokio::task::yield_now().await;
        for sending in [&source, &target] {
            for _ in 0..10 {
                tokio::time::advance(RELAY_IDLE_TIMEOUT / 2).await;
                sending.send(Ok(Message::Binary(vec![1].into()))).unwrap();
                tokio::task::yield_now().await;
                assert!(!idle.is_finished());
            }
        }
        tokio::time::advance(RELAY_IDLE_TIMEOUT).await;
        let error = idle.await.unwrap().unwrap_err();
        assert!(crate::error::is(&error, ErrorCode::ConnectTimeout));
    }

    #[tokio::test(start_paused = true)]
    async fn a_blocked_writer_has_a_separate_deadline() {
        let writer = Mutex::new(Box::pin(futures_util::sink::unfold(
            (),
            |(), _: Message| async { std::future::pending::<Result<(), axum::Error>>().await },
        )));
        let writing = forward(&writer, Message::Binary(vec![1].into()));
        tokio::pin!(writing);
        tokio::select! {
            result = &mut writing => assert!(result.is_err()),
            _ = tokio::time::sleep(RELAY_IDLE_TIMEOUT * 2) => panic!("blocked relay writer did not time out"),
        }
    }

    #[test]
    fn admission_reclaims_only_explicitly_cached_sessions() {
        let source = "127.0.0.1".parse().unwrap();
        let mut connections = Connections::default();
        let mut cancellations = Vec::new();
        for index in 0..MAX_SOURCE_SESSIONS {
            let (cancel, closed) = watch::channel(false);
            cancellations.push(closed);
            connections.sessions.insert(
                index.to_string(),
                Session {
                    network: "network".into(),
                    source,
                    device: Some("device".into()),
                    anonymous: false,
                    target: "target".into(),
                    generation: "generation".into(),
                    claim: None,
                    cancel,
                    cached_at: (index < 2)
                        .then(|| Instant::now() + std::time::Duration::from_secs(index as u64)),
                },
            );
        }
        connections
            .reserve_session("network", "target", source, Some("device"))
            .unwrap();
        assert!(*cancellations[0].borrow());
        assert!(!*cancellations[1].borrow());
        assert!(!*cancellations[2].borrow());
        for session in connections.sessions.values_mut() {
            session.cached_at = None;
        }
        // Returning the slot to active use consumes the source's last available slot.
        let (cancel, _) = watch::channel(false);
        connections.sessions.insert(
            "replacement".into(),
            Session {
                network: "network".into(),
                source,
                device: Some("device".into()),
                cached_at: None,
                anonymous: false,
                target: "target".into(),
                generation: "generation".into(),
                claim: None,
                cancel,
            },
        );
        let error = connections
            .reserve_session("network", "target", source, Some("device"))
            .unwrap_err();
        assert!(crate::error::is(&error, ErrorCode::SessionLimit));
        // An authenticated neighbor behind the same NAT has its own source budget.
        connections
            .reserve_session("network", "target", source, Some("neighbor"))
            .unwrap();
    }
}
