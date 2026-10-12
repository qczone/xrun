//! One authenticated callback per control generation; no business slot until claimed.
use super::*;
pub(super) struct Standby {
    pub generation: String,
    pub id: String,
    pub start: oneshot::Sender<String>,
}
pub(super) async fn route(
    State(app): State<Arc<App>>,
    Path((network, target, generation)): Path<(String, String, String)>,
    Extension(permit): Extension<TransportPermit>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> ApiResult<Response> {
    let selected = protocol(&headers)?;
    app.admission.check(&network, false)?;
    let upgrade = ws
        .max_message_size(MAX_MESSAGE)
        .max_frame_size(MAX_MESSAGE)
        .on_upgrade(move |ws| async move {
            let id = uuid::Uuid::new_v4().simple().to_string();
            let result = wait(&app, ws, &network, &target, &generation, &id, &permit).await;
            if let Err(error) = result {
                tracing::debug!(%error,"standby callback ended");
            }
            let mut connections = app.connections.lock().await;
            let key = (network, target);
            if connections
                .standby
                .get(&key)
                .is_some_and(|entry| entry.id == id)
            {
                connections.standby.remove(&key);
            }
        });
    Ok(negotiated(upgrade, selected))
}
async fn wait(
    app: &App,
    mut ws: WebSocket,
    network: &str,
    target: &str,
    generation: &str,
    id: &str,
    permit: &TransportPermit,
) -> Result<()> {
    let path = format!("/networks/{network}/standby/{target}/{generation}");
    let device = authenticate(app, &mut ws, network, &path, permit, false)
        .await?
        .context(ErrorCode::Unauthenticated.error("standby requires a member proof"))?
        .0;
    if device != target {
        bail!(ErrorCode::Unauthenticated.error("standby belongs to another device"));
    }
    let (start, claim) = oneshot::channel();
    let mut cancelled = {
        let mut connections = app.connections.lock().await;
        let key = (network.to_owned(), target.to_owned());
        let control = connections
            .controls
            .get(&key)
            .context(ErrorCode::DeviceOffline.error("target control is missing"))?;
        if control.generation != generation {
            bail!(ErrorCode::InvalidSession.error("standby control generation changed"));
        }
        let cancel = control.cancel.subscribe();
        if connections.standby.len() >= 512 || connections.standby.contains_key(&key) {
            bail!(ErrorCode::ConnectionLimit.error("standby callback capacity reached"));
        }
        connections.standby.insert(
            key,
            Standby {
                generation: generation.into(),
                id: id.into(),
                start,
            },
        );
        cancel
    };
    send(&mut ws, &RelayMessage::StandbyReady).await?;
    let sid = tokio::select! {
        sid=claim=>sid?,
        _=cancelled.changed()=>bail!(ErrorCode::ConnectionClosed.error("control replaced")),
        _=ws.next()=>bail!(ErrorCode::ConnectionClosed.error("standby closed")),
        _=tokio::time::sleep(RELAY_IDLE_TIMEOUT)=>bail!(ErrorCode::SessionExpired.error("standby expired")),
    };
    let sender = {
        let mut connections = app.connections.lock().await;
        let session = connections
            .sessions
            .get_mut(&sid)
            .context(ErrorCode::InvalidSession.error("standby reservation expired"))?;
        if session.network != network
            || session.target != target
            || session.generation != generation
        {
            bail!(ErrorCode::InvalidSession.error("standby reservation mismatch"));
        }
        session
            .claim
            .take()
            .context(ErrorCode::InvalidSession.error("standby already consumed"))?
    };
    send(
        &mut ws,
        &RelayMessage::Connected {
            flow_control: false,
        },
    )
    .await?;
    let _ = sender.send(Ok(ws));
    Ok(())
}
