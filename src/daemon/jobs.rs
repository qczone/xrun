//! Reliable job submission, source ownership, cancellation and log following.
use crate::error::ErrorCode;
use crate::{
    net::{self, Ws},
    protocol::*,
};
use anyhow::{Result, bail};
use futures_util::{SinkExt, StreamExt};
use std::sync::Arc;
use tokio_tungstenite::tungstenite::Message;

use super::Runtime;
use super::execution::submit;

pub(super) async fn serve(
    rt: Arc<Runtime>,
    source: &str,
    generation: u64,
    ws: &mut Ws,
    request: Request,
) -> Result<()> {
    match request {
        Request::Exec {
            execution,
            follow: following,
        } => {
            let input = net::receive_bytes(
                ws,
                execution.input_size,
                &execution.input_sha256,
                MAX_INPUT as u64,
            )
            .await?;
            rt.check_session(source, generation)?;
            let runtime = rt.clone();
            let source_device = source.to_owned();
            let job = tokio::task::spawn_blocking(move || {
                submit(runtime, &source_device, execution, input)
            })
            .await??;
            let id = job.job_id.clone();
            net::send(ws, &Data::Job { job }).await?;
            if following {
                follow(&rt, source, ws, &id, 0, true, true).await?;
            }
        }
        Request::Jobs {
            id,
            running,
            request_id,
            limit,
            offset,
        } => {
            if let Some(id) = id {
                let job = rt.owned_job(source, &id).await?;
                net::send(ws, &Data::Job { job }).await?
            } else {
                let jobs = rt
                    .store
                    .page_async(source, running, request_id.as_deref(), limit, offset)
                    .await?;
                net::send(ws, &Data::Jobs { jobs }).await?
            }
        }
        Request::Kill { id } => {
            {
                let job = rt.owned_job(source, &id).await?;
                let _gate = rt.gate.lock().unwrap();
                if !job.state.terminal() {
                    rt.canceled.lock().unwrap().insert(id.clone());
                }
            }
            follow(&rt, source, ws, &id, 0, false, true).await?
        }
        Request::Wait { id } => follow(&rt, source, ws, &id, 0, false, true).await?,
        Request::Logs {
            id,
            after,
            follow: following,
        } => follow(&rt, source, ws, &id, after, true, following).await?,
        _ => bail!(ErrorCode::InvalidRequest.error("operation dispatched to the wrong handler")),
    }
    Ok(())
}
async fn follow(
    rt: &Runtime,
    source: &str,
    ws: &mut Ws,
    id: &str,
    mut after: u64,
    logs: bool,
    following: bool,
) -> Result<()> {
    let mut ping = tokio::time::Instant::now();
    let mut previous = None;
    let mut changes = rt.store.subscribe();
    let snapshot = if following {
        None
    } else {
        Some(rt.owned_job(source, id).await?.last_seq)
    };
    loop {
        // Mark the notification before reading so a concurrent write cannot
        // be lost between the database snapshot and the wait below.
        changes.borrow_and_update();
        rt.allow(source)?;
        let job = rt.owned_job(source, id).await?;
        let mut events = if logs {
            rt.store.logs_async(id, after).await?
        } else {
            vec![]
        };
        if let Some(last) = snapshot {
            events.retain(|e| e.seq <= last);
        }
        let count = events.len();
        if let Some(e) = events.last() {
            after = e.seq;
        }
        let state = (
            job.state.clone(),
            job.output_complete,
            job.incomplete_reason.clone(),
        );
        if logs && (count > 0 || previous.as_ref() != Some(&state)) {
            net::send(
                ws,
                &Data::Logs {
                    events,
                    job: job.clone(),
                },
            )
            .await?;
        } else if job.state.terminal() && !logs {
            net::send(ws, &Data::Job { job: job.clone() }).await?;
        }
        previous = Some(state);
        if (!following && count < 16) || (job.state.terminal() && (!logs || count < 16)) {
            if logs {
                net::send(ws, &Data::End).await?
            }
            return Ok(());
        }
        if ping.elapsed() > HEARTBEAT_INTERVAL {
            ws.send(Message::Ping(vec![].into())).await?;
            ping = tokio::time::Instant::now();
        }
        if count == 16 {
            continue;
        }
        tokio::select! {
            _ = changes.changed() => {},
            _ = tokio::time::sleep_until(ping + HEARTBEAT_INTERVAL) => {},
            m = ws.next() => match m {
                Some(Ok(Message::Ping(b))) => ws.send(Message::Pong(b)).await?,
                Some(Ok(Message::Pong(_))) => {},
                _ => bail!(ErrorCode::ConnectionClosed.error("subscriber disconnected")),
            }
        }
    }
}
