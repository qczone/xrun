//! One authorization and audit boundary before dispatching any operation.
use crate::{net::Ws, protocol::*};
use anyhow::Result;
use std::sync::Arc;

use super::{FileAudit, Runtime, files, jobs, streams};

pub(super) async fn serve(
    rt: Arc<Runtime>,
    source: &str,
    generation: u64,
    ws: &mut Ws,
    request: Request,
) -> Result<()> {
    rt.check_session(source, generation)?;
    let file_op = match &request {
        Request::Push { path, .. } => Some(("push", Some(path.clone()))),
        Request::Pull { path, .. } => Some(("pull", Some(path.clone()))),
        Request::Screenshot => Some(("screenshot", None)),
        Request::Forward { port } => Some(("forward", Some(format!("localhost:{port}")))),
        Request::StreamExec { execution } => Some(("stream_exec", Some(execution.program.clone()))),
        _ => None,
    };
    let mut audit = file_op.map(|(op, path)| FileAudit {
        store: rt.store.clone(),
        value: serde_json::json!({"source_device_id":source,"op":op,"path":path,"size":null,"started_at_ms":now_ms()}),
        completed: false,
        stream_counts: None,
    });
    match request {
        request @ (Request::StreamExec { .. } | Request::Forward { .. }) => {
            streams::serve(rt, source, generation, ws, request, &mut audit).await?
        }
        request @ (Request::Push { .. } | Request::Pull { .. } | Request::Screenshot) => {
            files::serve(rt, source, generation, ws, request, &mut audit).await?
        }
        request @ (Request::Exec { .. }
        | Request::Jobs { .. }
        | Request::Kill { .. }
        | Request::Wait { .. }
        | Request::Logs { .. }) => jobs::serve(rt, source, generation, ws, request).await?,
    }
    Ok(())
}
