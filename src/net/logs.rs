//! Binary output batches and minimal mutable job updates on the encrypted channel.
use super::*;
use base64::{Engine, engine::general_purpose::STANDARD};
use std::collections::BTreeMap;

const MAGIC: &[u8; 4] = b"XRL1";
const MUTABLE: &[&str] = &[
    "state",
    "result",
    "last_log_seq",
    "log_bytes",
    "output_complete",
    "output_loss_reason",
    "error_code",
    "error_message",
    "started_at_ms",
    "finished_at_ms",
    "updated_at_ms",
    "leftover_possible",
    "process",
    "attachments",
];
#[derive(Default)]
pub(crate) struct LogState {
    sent: Option<serde_json::Value>,
    received: Option<Job>,
    after: u64,
}
impl LogState {
    pub(crate) fn reset_sender(&mut self) {
        self.sent = None;
    }
}
/// Send raw stdout/stderr bytes. JSON adapters keep base64 only at the local API boundary.
pub async fn send_logs(ws: &mut Ws, events: Vec<LogEvent>, job: &Job) -> Result<()> {
    let current = serde_json::to_value(job)?;
    match ws.get_ref().logs.sent.as_ref() {
        Some(old) if old["job_id"] == current["job_id"] && old["db_id"] == current["db_id"] => {
            let fields: BTreeMap<_, _> = MUTABLE
                .iter()
                .filter(|key| old[**key] != current[**key])
                .map(|key| ((*key).to_owned(), current[*key].clone()))
                .collect();
            if !fields.is_empty() {
                send(
                    ws,
                    &Data::LogUpdate {
                        job_id: job.job_id.clone(),
                        db_id: job.db_id.clone(),
                        fields,
                    },
                )
                .await?;
            }
        }
        _ => send(ws, &Data::LogSnapshot { job: job.clone() }).await?,
    }
    ws.get_mut().logs.sent = Some(current);
    let mut batch = Vec::new();
    let mut bytes = 8 + job.job_id.len();
    for event in events {
        let raw = STANDARD.decode(&event.data_base64)?;
        if !matches!(event.stream.as_str(), "stdout" | "stderr") || raw.len() > FILE_CHUNK {
            bail!(ErrorCode::InvalidMessage.error("invalid output record"));
        }
        if !batch.is_empty() && (batch.len() == 16 || bytes + 13 + raw.len() > MAX_MESSAGE) {
            ws.send(Message::Binary(encode(&job.job_id, &batch)?.into()))
                .await?;
            batch.clear();
            bytes = 8 + job.job_id.len();
        }
        bytes += 13 + raw.len();
        batch.push((event.seq, event.stream == "stderr", raw));
    }
    ws.send(Message::Binary(encode(&job.job_id, &batch)?.into()))
        .await?;
    Ok(())
}
fn encode(id: &str, events: &[(u64, bool, Vec<u8>)]) -> Result<Vec<u8>> {
    if id.is_empty() || id.len() > 128 || events.len() > 16 {
        bail!(ErrorCode::InvalidMessage.error("invalid output batch identity"));
    }
    let mut bytes = Vec::new();
    bytes.extend_from_slice(MAGIC);
    bytes.extend_from_slice(&(id.len() as u16).to_be_bytes());
    bytes.extend_from_slice(id.as_bytes());
    bytes.extend_from_slice(&(events.len() as u16).to_be_bytes());
    for (seq, stderr, raw) in events {
        bytes.extend_from_slice(&seq.to_be_bytes());
        bytes.push(u8::from(*stderr));
        bytes.extend_from_slice(&(raw.len() as u32).to_be_bytes());
        bytes.extend_from_slice(raw);
    }
    if bytes.len() > MAX_MESSAGE {
        bail!(ErrorCode::MessageTooLarge.error("output batch exceeds limit"));
    }
    Ok(bytes)
}
pub(super) fn update(io: &mut SocketIo, text: &str) -> Result<bool> {
    let tag: serde_json::Value = serde_json::from_str(text)?;
    if tag["type"] == "logs" {
        bail!(
            ErrorCode::VersionMismatch.error("JSON log bodies are not supported by this protocol")
        );
    }
    if !matches!(tag["type"].as_str(), Some("log_snapshot" | "log_update")) {
        return Ok(false);
    }
    match serde_json::from_str(text)? {
        Data::LogSnapshot { job } => {
            if job.kind() != JobKind::Exec {
                bail!(ErrorCode::InvalidMessage.error("output snapshot is not a command"));
            }
            io.logs.received = Some(job);
            io.logs.after = 0;
        }
        Data::LogUpdate {
            job_id,
            db_id,
            fields,
        } => {
            let old = io
                .logs
                .received
                .as_ref()
                .context(ErrorCode::InvalidMessage.error("output update lacks a snapshot"))?;
            if old.job_id != job_id
                || old.db_id != db_id
                || fields.keys().any(|key| !MUTABLE.contains(&key.as_str()))
            {
                bail!(
                    ErrorCode::InvalidMessage.error("output update changes its immutable binding")
                );
            }
            let mut value = serde_json::to_value(old)?;
            for (key, field) in fields {
                value[key] = field;
            }
            let new: Job = serde_json::from_value(value)?;
            if new.last_log_seq < old.last_log_seq
                || (old.state.terminal() && new.state != old.state)
            {
                bail!(ErrorCode::InvalidMessage.error("output update regresses state"));
            }
            io.logs.received = Some(new);
        }
        _ => unreachable!(),
    }
    Ok(true)
}
fn take<'a>(bytes: &mut &'a [u8], count: usize) -> Result<&'a [u8]> {
    if bytes.len() < count {
        bail!(ErrorCode::InvalidMessage.error("truncated output frame"));
    }
    let (head, tail) = bytes.split_at(count);
    *bytes = tail;
    Ok(head)
}
pub(super) fn receive(io: &mut SocketIo, mut bytes: &[u8]) -> Result<Data> {
    if bytes.len() > MAX_MESSAGE || take(&mut bytes, 4)? != MAGIC {
        bail!(ErrorCode::InvalidMessage.error("expected binary output batch"));
    }
    let id_len = u16::from_be_bytes(take(&mut bytes, 2)?.try_into()?) as usize;
    if id_len == 0 || id_len > 128 {
        bail!(ErrorCode::InvalidMessage.error("invalid output identity length"));
    }
    let id = std::str::from_utf8(take(&mut bytes, id_len)?)?;
    let job = io
        .logs
        .received
        .as_ref()
        .context(ErrorCode::InvalidMessage.error("output lacks a snapshot"))?;
    if job.job_id != id {
        bail!(ErrorCode::InvalidMessage.error("output belongs to another job"));
    }
    let count = u16::from_be_bytes(take(&mut bytes, 2)?.try_into()?) as usize;
    if count > 16 {
        bail!(ErrorCode::InvalidMessage.error("too many output records"));
    }
    let mut events = Vec::with_capacity(count);
    let mut after = io.logs.after;
    for _ in 0..count {
        let seq = u64::from_be_bytes(take(&mut bytes, 8)?.try_into()?);
        let stream = match take(&mut bytes, 1)?[0] {
            0 => "stdout",
            1 => "stderr",
            _ => bail!(ErrorCode::InvalidMessage.error("invalid output stream")),
        };
        let len = u32::from_be_bytes(take(&mut bytes, 4)?.try_into()?) as usize;
        if len > FILE_CHUNK || seq <= after || seq > job.last_log_seq {
            bail!(ErrorCode::InvalidMessage.error("invalid output sequence or size"));
        }
        events.push(LogEvent {
            seq,
            stream: stream.into(),
            data_base64: STANDARD.encode(take(&mut bytes, len)?),
        });
        after = seq;
    }
    if !bytes.is_empty() {
        bail!(ErrorCode::InvalidMessage.error("trailing output bytes"));
    }
    io.logs.after = after;
    Ok(Data::Logs {
        events,
        job: job.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn snapshot() -> Job {
        let mut job = Job::accepted(
            "source",
            "target",
            &JobContext::new("database"),
            "digest".into(),
            JobDetails::Exec(CommandParams {
                program: "echo".into(),
                args: vec!["immutable".into()],
                cwd: "/tmp/owned".into(),
                timeout: 1,
                shell: None,
                input_size: Some(0),
                input_sha256: Some(sha256(&[])),
            }),
        );
        job.last_log_seq = 3;
        job
    }
    #[tokio::test]
    async fn successive_batches_send_one_snapshot_and_only_changed_fields() -> Result<()> {
        use tokio_tungstenite::tungstenite::protocol::Role;
        let (a, b) = tokio::io::duplex(MAX_MESSAGE * 2);
        let mut sender =
            WebSocketStream::from_raw_socket(SocketIo::new(Box::new(a)), Role::Server, None).await;
        let mut receiver =
            WebSocketStream::from_raw_socket(SocketIo::new(Box::new(b)), Role::Client, None).await;
        let mut job = snapshot();
        let event = |seq| LogEvent {
            seq,
            stream: "stdout".into(),
            data_base64: STANDARD.encode([0, 0xff, 0xe4, 0xb8, 0xad]),
        };
        send_logs(&mut sender, vec![event(1)], &job).await?;
        job.log_bytes = 10;
        send_logs(&mut sender, vec![event(2)], &job).await?;
        let Message::Text(first) = receiver.next().await.unwrap()? else {
            bail!("snapshot")
        };
        assert!(matches!(
            serde_json::from_str(&first)?,
            Data::LogSnapshot { .. }
        ));
        let Message::Binary(raw) = receiver.next().await.unwrap()? else {
            bail!("binary output")
        };
        assert!(raw.ends_with(&[0, 0xff, 0xe4, 0xb8, 0xad]));
        let Message::Text(delta) = receiver.next().await.unwrap()? else {
            bail!("delta")
        };
        let Data::LogUpdate { fields, .. } = serde_json::from_str(&delta)? else {
            bail!("mutable update")
        };
        assert_eq!(fields.len(), 1);
        assert_eq!(fields["log_bytes"], 10);
        assert!(!delta.contains("immutable"));
        assert!(matches!(
            receiver.next().await.unwrap()?,
            Message::Binary(_)
        ));
        Ok(())
    }
    #[test]
    fn raw_binary_roundtrips_and_rejects_spliced_or_malformed_batches() -> Result<()> {
        let (a, _b) = tokio::io::duplex(64);
        let mut io = SocketIo::new(Box::new(a));
        let job = snapshot();
        assert!(update(
            &mut io,
            &serde_json::to_string(&Data::LogSnapshot { job: job.clone() })?
        )?);
        let frame = encode(
            &job.job_id,
            &[
                (1, false, vec![0, 255, 0xe4]),
                (2, true, vec![0xb8, 0xad, 10]),
            ],
        )?;
        let Data::Logs { events, .. } = receive(&mut io, &frame)? else {
            unreachable!()
        };
        assert_eq!(STANDARD.decode(&events[0].data_base64)?, [0, 255, 0xe4]);
        assert_eq!(events[1].stream, "stderr");
        assert!(receive(&mut io, &frame).is_err());
        assert!(receive(&mut io, &encode("other", &[])?).is_err());
        assert!(receive(&mut io, &frame[..frame.len() - 1]).is_err());
        let delta = Data::LogUpdate {
            job_id: job.job_id.clone(),
            db_id: job.db_id.clone(),
            fields: BTreeMap::from([("params".into(), serde_json::json!({}))]),
        };
        assert!(update(&mut io, &serde_json::to_string(&delta)?).is_err());
        Ok(())
    }
}
