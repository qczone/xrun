//! An operation session. A completed request may be returned to the local
//! daemon; dropping a session at any other point discards the connection.
use crate::error::ErrorCode;
use crate::{
    config::Identity,
    net::{self, Ws},
    protocol::*,
};
use anyhow::{Result, bail};
use futures_util::SinkExt;
use std::time::Duration;
use tokio_tungstenite::tungstenite::Message;

pub(crate) struct Session {
    pub ws: Ws,
    pub db_id: String,
    pub cwd: String,
    protocol: u32,
    pooled: bool,
    binding: Option<SubmissionBinding>,
    submission: Option<crate::store::Submission>,
    submission_store: Option<crate::store::SubmissionStore>,
}
struct SubmissionBinding {
    source: String,
    target: String,
    target_name: String,
    ca_pin: String,
}
impl Session {
    pub(crate) async fn open(id: &Identity, target: &str) -> Result<Self> {
        let mut session = if let Some(ws) = crate::ipc::connect(id, target).await? {
            Self::ready(ws, target, true).await?
        } else {
            let (ws, _) = crate::network::session(id, target).await?;
            Self::ready(ws, target, false).await?
        };
        session.binding = Some(SubmissionBinding {
            source: id.device_id.clone(),
            target: target.into(),
            target_name: crate::network::current(id)?.member(target)?.name.clone(),
            ca_pin: crate::crypto::ca_spki_pin(&id.ca_pem)?,
        });
        Ok(session)
    }
    async fn ready(mut ws: Ws, target: &str, pooled: bool) -> Result<Self> {
        match tokio::time::timeout(Duration::from_secs(30), net::receive::<Data>(&mut ws)).await?? {
            Data::Ready {
                version: _,
                protocol,
                selected_protocol,
                device_id,
                db_id,
                default_cwd,
            } => {
                ProtocolRange::CURRENT.confirm(protocol, selected_protocol)?;
                if device_id != target {
                    bail!(
                        ErrorCode::DeviceMismatch.error("session connected to a different device")
                    );
                }
                Ok(Self {
                    ws,
                    db_id,
                    cwd: default_cwd,
                    protocol: selected_protocol,
                    pooled,
                    binding: None,
                    submission: None,
                    submission_store: None,
                })
            }
            Data::Error { code, message } => {
                bail!(crate::error::CodedError::from_wire(code, message))
            }
            _ => bail!(ErrorCode::InvalidMessage.error("expected ready")),
        }
    }
    pub(crate) async fn send_request(&mut self, request: Request) -> Result<()> {
        let message = self.prepare_request(request.clone())?;
        if !matches!(request, Request::Exec { .. })
            && let Some(context) = request.job_context()
            && let Some(binding) = &self.binding
        {
            let record = crate::store::Submission {
                request_id: context.request_id,
                source_device_id: binding.source.clone(),
                target_device_id: binding.target.clone(),
                target_name: binding.target_name.clone(),
                ca_pin: binding.ca_pin.clone(),
                db_id: context.db_id,
                request_hash: request.job_hash().expect("business intent"),
                kind: request.job_details().expect("business details").kind(),
                label: request.job_details().expect("business details").label(),
                created_at_ms: now_ms(),
                job_id: None,
                status: "unconfirmed".into(),
            };
            let store = crate::store::SubmissionStore::open(
                &crate::config::device_dir()?.join("submissions.sqlite"),
            )?;
            store.save(&record)?;
            self.submission_store = Some(store);
            self.submission = Some(record);
        }
        self.ws.send(message).await?;
        Ok(())
    }
    pub(crate) async fn accept_operation(&mut self) -> Result<Job> {
        match net::receive(&mut self.ws).await? {
            Data::Accepted { job, fresh } => {
                if let Some(record) = self.submission.as_mut() {
                    if record.request_id != job.request_id
                        || record.db_id != job.db_id
                        || record.target_device_id != job.target_device_id
                        || record.source_device_id != job.source_device_id
                        || record.request_hash != job.request_hash
                        || record.kind != job.kind()
                    {
                        bail!(
                            ErrorCode::InvalidMessage.error(
                                "job acknowledgement does not match the submitted operation"
                            )
                        );
                    }
                    record.job_id = Some(job.job_id.clone());
                    record.status = "accepted".into();
                    self.submission_store
                        .as_ref()
                        .expect("submission store")
                        .save(record)?;
                }
                if !fresh {
                    bail!(ErrorCode::Unconfirmed.error(format!(
                        "request already accepted as {}; query its result instead of replaying the data stream",
                        job.job_id
                    )));
                }
                Ok(job)
            }
            Data::Error { code, message } => {
                if let Some(record) = self.submission.as_mut() {
                    if ErrorCode::from_wire(code.clone()).rejects_submission() {
                        record.status = "not_accepted".into();
                    }
                    self.submission_store
                        .as_ref()
                        .expect("submission store")
                        .save(record)?;
                }
                bail!(crate::error::CodedError::from_wire(code, message))
            }
            _ => bail!(ErrorCode::InvalidMessage.error("expected operation admission")),
        }
    }
    // Complete local validation before the caller records a possibly sent request.
    pub(crate) fn prepare_request(&self, request: Request) -> Result<Message> {
        encode_request(self.protocol, request.minimum_protocol(), request)
    }
    // Recycling is optional. A confirmed operation must not become a failure
    // just because the daemon/socket disappears after the final response.
    pub(crate) async fn finish(mut self) {
        let result = tokio::time::timeout(Duration::from_secs(2), async {
            if !matches!(net::receive::<Data>(&mut self.ws).await?, Data::Complete) {
                bail!(ErrorCode::InvalidMessage.error("expected request completion"));
            }
            if self.pooled {
                net::send(&mut self.ws, &crate::ipc::LocalRequest::Release).await?;
                if !matches!(
                    net::receive::<crate::ipc::LocalResponse>(&mut self.ws).await?,
                    crate::ipc::LocalResponse::Released
                ) {
                    bail!(ErrorCode::InvalidMessage.error("expected cache acknowledgement"));
                }
            }
            Ok::<_, anyhow::Error>(())
        })
        .await;
        if !matches!(result, Ok(Ok(()))) {
            tracing::debug!("completed session was not cached");
        }
    }
}

fn encode_request(protocol: u32, required: u32, request: Request) -> Result<Message> {
    ProtocolRange::require(protocol, required)?;
    let text = serde_json::to_string(&Data::Request { request })?;
    if text.len() > MAX_MESSAGE {
        bail!(ErrorCode::MessageTooLarge.error("request header limit exceeded"));
    }
    Ok(Message::Text(text.into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unsupported_future_operation_fails_before_it_can_be_sent() {
        let request = || Request::Forward {
            port: 1234,
            context: crate::protocol::JobContext::new("db"),
        };
        let error = encode_request(1, 2, request()).unwrap_err();
        assert!(crate::error::is(&error, ErrorCode::VersionMismatch));
        let Message::Text(text) = encode_request(1, 1, request()).unwrap() else {
            panic!("request header must be text");
        };
        assert!(matches!(
            serde_json::from_str::<Data>(&text).unwrap(),
            Data::Request {
                request: Request::Forward { port: 1234, .. }
            }
        ));
    }
}
