//! Shared session, diagnostics, input and job-result helpers.
use crate::error::ErrorCode;
use crate::{
    config::Identity,
    net::{self, Ws},
    protocol::*,
    session::Session,
};
use anyhow::{Result, bail};
use serde::Serialize;
use std::io::Read;

pub(super) fn print<T: Serialize>(json: bool, value: &T, text: impl FnOnce()) {
    if json {
        println!("{}", serde_json::to_string(value).unwrap())
    } else {
        text()
    }
}
pub(super) fn diagnostic(json: bool, error: &anyhow::Error) {
    if json {
        eprintln!("{}", serde_json::to_string(&Data::error(error)).unwrap())
    } else {
        eprintln!("[xrun] {error:#}")
    }
}
pub(super) fn require_linux() -> Result<()> {
    if !cfg!(target_os = "linux") {
        bail!(ErrorCode::UnsupportedPlatform.error("Server deployment requires Linux"))
    }
    Ok(())
}
pub(super) async fn identity() -> Result<Identity> {
    let mut id = Identity::load()?;
    net::renew_identity(&mut id).await?;
    Ok(id)
}
pub(super) async fn session(id: &Identity, target: &str) -> Result<Session> {
    Session::open(id, target).await
}
pub(super) async fn response(ws: &mut Ws) -> Result<Data> {
    match net::receive::<Data>(ws).await? {
        Data::Error { code, message } => bail!(crate::error::CodedError::from_wire(code, message)),
        value => Ok(value),
    }
}
pub(super) async fn request(id: &Identity, target: &str, req: Request) -> Result<Data> {
    let mut s = session(id, target).await?;
    s.send_request(req).await?;
    let value = response(&mut s.ws).await?;
    s.finish().await;
    Ok(value)
}
pub(super) fn job_ref(job: &Job) -> String {
    format!("{}/{}", job.target_device_id, job.job_id)
}
pub(super) fn parse_job(value: &str, target: &str, name: &str) -> Result<String> {
    let id = if let Some((device, id)) = value.split_once('/') {
        if device != target && device != name {
            bail!(ErrorCode::InvalidJobRef.error("job belongs to another device"))
        }
        id
    } else {
        value
    };
    let id = id.to_ascii_uppercase();
    if id.len() != 6
        || !id
            .bytes()
            .all(|b| b"0123456789ABCDEFGHJKMNPQRSTVWXYZ".contains(&b))
    {
        bail!(ErrorCode::InvalidJobRef.error("expected a six-character job ID"))
    };
    Ok(id)
}
pub(super) fn job_code(job: &Job) -> i32 {
    match job.state {
        JobState::TimedOut => 124,
        JobState::Canceled => 130,
        JobState::Failed | JobState::Lost => 125,
        _ => {
            if let Some(signal) = job.signal {
                128 + signal
            } else {
                let code = job.exit_code.unwrap_or(125);
                if (0..=255).contains(&code) {
                    code as i32
                } else {
                    eprintln!("[xrun] remote exit code {code}");
                    1
                }
            }
        }
    }
}
pub(super) fn show_job(json: bool, job: &Job) {
    print(json, job, || {
        println!("{}\t{:?}\t{}", job_ref(job), job.state, job.program)
    });
}
pub(super) fn read_input(max: u64) -> Result<Vec<u8>> {
    let mut bytes = vec![];
    std::io::stdin().take(max + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max {
        bail!(ErrorCode::InputTooLarge.error(format!("maximum {max} bytes")))
    }
    Ok(bytes)
}
pub(super) fn network_error(error: &anyhow::Error) -> bool {
    crate::error::code(error).is_some_and(|code| code.is_network())
        || error.chain().any(|e| {
            e.downcast_ref::<reqwest::Error>()
                .is_some_and(|e| e.is_connect() || e.is_timeout())
                || e.downcast_ref::<tokio_tungstenite::tungstenite::Error>()
                    .is_some()
        })
}
pub(super) fn definitive(error: &anyhow::Error) -> bool {
    crate::error::code(error).is_some_and(|code| code.rejects_submission())
}
pub(super) async fn termination() {
    #[cfg(unix)]
    {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("TERM handler");
        let mut hup = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup())
            .expect("HUP handler");
        tokio::select! {_=term.recv()=>{},_=hup.recv()=>{}}
    }
    #[cfg(not(unix))]
    {
        std::future::pending::<()>().await;
    }
}
pub(super) async fn wait(id: &Identity, target: &str, job: &str) -> Result<Job> {
    match request(id, target, Request::Wait { id: job.into() }).await? {
        Data::Job { job } => Ok(job),
        _ => bail!(ErrorCode::InvalidMessage.error("expected final job state")),
    }
}
