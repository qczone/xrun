//! Connection-bound execution and TCP forwarding.
use crate::error::ErrorCode;
use crate::{
    net::{self, Ws},
    protocol::*,
};
use anyhow::{Context, Result, bail};
use std::{
    path::PathBuf,
    sync::{Arc, atomic::Ordering},
};

use super::execution::check_capacity;
use super::program::{resolve_program, validate_stream};
use super::{FileAudit, RunningStream, Runtime};

pub(super) async fn serve(
    rt: Arc<Runtime>,
    source: &str,
    generation: u64,
    ws: &mut Ws,
    request: Request,
    audit: &mut Option<FileAudit>,
) -> Result<()> {
    match request {
        Request::StreamExec { execution } => {
            let counts = Arc::new(crate::streaming::Counts::default());
            if let Some(a) = audit.as_mut() {
                a.stream_counts = Some(counts.clone());
            }
            let cfg = rt.config()?;
            validate_stream(&execution)?;
            let cwd = PathBuf::from(&execution.cwd);
            let mut env = cfg.env;
            for (k, v) in &execution.env {
                #[cfg(windows)]
                env.retain(|key, _| !key.eq_ignore_ascii_case(k));
                env.insert(k.clone(), v.clone());
            }
            let program = resolve_program(&execution.program, &cwd, &env)?;
            let id = format!("stream_{}", uuid::Uuid::new_v4());
            let child = {
                let _gate = rt.gate.lock().unwrap();
                rt.check_session(source, generation)?;
                if rt.stopping.load(Ordering::SeqCst) {
                    bail!(ErrorCode::DaemonStopping.error("daemon shutting down"))
                }
                check_capacity(&rt)?;
                let child =
                    crate::process::spawn(&program, &execution.args, &cwd, &env, &id, false)?;
                rt.running.lock().unwrap().insert(id.clone(), child.pid);
                rt.streams.fetch_add(1, Ordering::SeqCst);
                child
            };
            let running = RunningStream { rt: rt.clone(), id };
            if let Some(a) = audit.as_mut() {
                a.value["args"] = serde_json::json!(execution.args);
                a.value["cwd"] = serde_json::json!(execution.cwd);
                a.value["started_at_ms"] = serde_json::json!(now_ms());
            }
            let outcome =
                crate::streaming::serve(ws, child, execution.timeout, counts, move || {
                    drop(running)
                })
                .await?;
            if let Some(a) = audit.as_mut() {
                a.completed = true;
                a.value["outcome"] = serde_json::to_value(outcome)?;
            }
        }
        Request::Forward { port } => {
            let _permit = rt
                .forwards
                .clone()
                .try_acquire_owned()
                .context(ErrorCode::DeviceBusy.error("too many forwarded connections"))?;
            let tcp = crate::forwarding::connect_loopback(port).await?;
            rt.check_session(source, generation)?;
            net::send(ws, &Data::ForwardReady { port }).await?;
            crate::forwarding::bridge(ws, tcp).await?;
            if let Some(a) = audit.as_mut() {
                a.completed = true;
            }
        }
        _ => bail!(ErrorCode::InvalidRequest.error("operation dispatched to the wrong handler")),
    }
    Ok(())
}
