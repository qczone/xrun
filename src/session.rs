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
}
impl Session {
    pub(crate) async fn open(id: &Identity, target: &str) -> Result<Self> {
        if let Some(ws) = crate::ipc::connect(id, target).await? {
            Self::ready(ws, target, true).await
        } else {
            let (ws, _) = crate::network::session(id, target).await?;
            Self::ready(ws, target, false).await
        }
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
                })
            }
            Data::Error { code, message } => {
                bail!(crate::error::CodedError::from_wire(code, message))
            }
            _ => bail!(ErrorCode::InvalidMessage.error("expected ready")),
        }
    }
    pub(crate) async fn send_request(&mut self, request: Request) -> Result<()> {
        let message = self.prepare_request(request)?;
        self.ws.send(message).await?;
        Ok(())
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
        let request = || Request::Forward { port: 1234 };
        let error = encode_request(1, 2, request()).unwrap_err();
        assert!(crate::error::is(&error, ErrorCode::VersionMismatch));
        let Message::Text(text) = encode_request(1, 1, request()).unwrap() else {
            panic!("request header must be text");
        };
        assert!(matches!(
            serde_json::from_str::<Data>(&text).unwrap(),
            Data::Request {
                request: Request::Forward { port: 1234 }
            }
        ));
    }
}
