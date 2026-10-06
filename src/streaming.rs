use crate::error::ErrorCode;
use crate::{
    net::{self, Ws},
    process::{self, ManagedChild, Output},
    protocol::*,
};
use anyhow::{Context, Result, bail};
use futures_util::{SinkExt, StreamExt};
use serde::Serialize;
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::{mpsc, watch},
};
use tokio_tungstenite::tungstenite::Message;

#[derive(Serialize)]
pub(crate) struct Outcome {
    pub result: StreamResult,
    pub input_bytes: u64,
    pub stdout_bytes: u64,
    pub stderr_bytes: u64,
}
#[derive(Default)]
pub(crate) struct Counts {
    pub input: AtomicU64,
    pub stdout: AtomicU64,
    pub stderr: AtomicU64,
}
impl Counts {
    pub(crate) fn snapshot(&self) -> serde_json::Value {
        serde_json::json!({"stdin_bytes_received":self.input.load(Ordering::Relaxed),
            "stdout_bytes_read":self.stdout.load(Ordering::Relaxed),
            "stderr_bytes_read":self.stderr.load(Ordering::Relaxed)})
    }
}

fn frame(value: &Data) -> Result<Message> {
    Ok(Message::Text(serde_json::to_string(value)?.into()))
}

async fn writer<S>(mut sink: S, mut rx: mpsc::Receiver<Message>) -> Result<()>
where
    S: futures_util::Sink<Message, Error = tokio_tungstenite::tungstenite::Error> + Unpin,
{
    let mut ping = tokio::time::interval(Duration::from_secs(15));
    loop {
        let message = tokio::select! {
            message = rx.recv() => match message { Some(m) => m, None => return Ok(()) },
            _ = ping.tick() => Message::Ping(vec![].into()),
        };
        tokio::time::timeout(Duration::from_secs(300), sink.send(message))
            .await
            .context(ErrorCode::StreamTimeout.error("receiver is not reading"))??;
    }
}

async fn drain(
    mut pipe: Output,
    stream: u8,
    tx: mpsc::Sender<Message>,
    counts: Arc<Counts>,
) -> Result<u64> {
    let mut buffer = vec![0; FILE_CHUNK + 1];
    buffer[0] = stream;
    let mut total = 0;
    loop {
        let n = pipe.read(&mut buffer[1..]).await?;
        if n == 0 {
            return Ok(total);
        }
        (if stream == 1 {
            &counts.stdout
        } else {
            &counts.stderr
        })
        .fetch_add(n as u64, Ordering::Relaxed);
        tx.send(Message::Binary(buffer[..n + 1].to_vec().into()))
            .await
            .context(ErrorCode::ConnectionClosed.error("stream writer stopped"))?;
        total += n as u64;
    }
}

pub(crate) async fn serve(
    ws: &mut Ws,
    mut child: ManagedChild,
    timeout: u64,
    counts: Arc<Counts>,
    on_finished: impl FnOnce() + Send,
) -> Result<Outcome> {
    net::send(ws, &Data::StreamReady).await?;
    let (socket_tx, mut socket_rx) = ws.split();
    let (tx, rx) = mpsc::channel::<Message>(8);
    let (exited, mut input_exited) = watch::channel(false);
    let (drained, mut output_drained) = watch::channel(false);
    let exit_sent = Arc::new(AtomicBool::new(false));
    let receiver_exit = exit_sent.clone();
    let mut stdin = child.stdin.take();
    let stdout = child.stdout.take().context("missing child stdout")?;
    let stderr = child.stderr.take().context("missing child stderr")?;
    let input_tx = tx.clone();
    let input_counts = counts.clone();
    let receive = async move {
        let mut total = 0;
        let mut eof = false;
        while let Some(message) = socket_rx.next().await {
            if *input_exited.borrow() {
                stdin = None;
            }
            match message? {
                Message::Binary(bytes) if !eof && bytes.len() <= FILE_CHUNK => {
                    total += bytes.len() as u64;
                    input_counts
                        .input
                        .fetch_add(bytes.len() as u64, Ordering::Relaxed);
                    if let Some(pipe) = &mut stdin {
                        let result = tokio::select! {
                            r=pipe.write_all(&bytes)=>Some(r),
                            _=input_exited.changed()=>None,
                        };
                        match result {
                            Some(Ok(())) => {}
                            Some(Err(e)) if e.kind() != std::io::ErrorKind::BrokenPipe => {
                                return Err(e.into());
                            }
                            _ => stdin = None,
                        }
                    }
                }
                Message::Text(text) => match serde_json::from_str::<Data>(&text)? {
                    Data::End if !eof => {
                        eof = true;
                        stdin = None;
                    }
                    Data::StreamExitAck if receiver_exit.load(Ordering::Relaxed) => {
                        return Ok::<_, anyhow::Error>(total);
                    }
                    _ => bail!(
                        ErrorCode::InvalidMessage
                            .error("expected stdin EOF or stream result acknowledgement")
                    ),
                },
                Message::Ping(bytes) => input_tx
                    .send(Message::Pong(bytes))
                    .await
                    .context(ErrorCode::ConnectionClosed.error("stream writer stopped"))?,
                Message::Pong(_) => {}
                Message::Close(_) => break,
                _ => bail!(ErrorCode::InvalidMessage.error("invalid stdin chunk")),
            }
        }
        bail!(ErrorCode::ConnectionClosed.error("streaming process disconnected"))
    };
    let start = crate::clock::elapsed_clock_ms()?;
    let pid = child.pid;
    let output_tx = tx.clone();
    let output = async move {
        let counts = tokio::try_join!(
            drain(stdout, 1, output_tx.clone(), counts.clone()),
            drain(stderr, 2, output_tx, counts)
        )?;
        let _ = drained.send(true);
        Ok::<_, anyhow::Error>(counts)
    };
    let completion = async {
        let wait = async {
            let mut timed_out = false;
            let status = loop {
                tokio::select! {
                    r=child.wait()=>break r?,
                    _=tokio::time::sleep(Duration::from_millis(100))=>{
                        if timeout>0 && crate::clock::elapsed_clock_ms()?.saturating_sub(start)>=timeout.saturating_mul(1000) {
                            timed_out = true;
                            process::terminate(pid);
                            break match tokio::time::timeout(Duration::from_secs(5),child.wait()).await {
                                Ok(r)=>r?, Err(_)=>{process::force_kill(pid); child.wait().await?},
                            };
                        }
                    }
                }
            };
            let _ = exited.send(true);
            process::terminate(pid);
            // Kill descendants that keep output pipes open, while the unreaped
            // leader still protects its process-group ID from reuse.
            if !*output_drained.borrow() {
                let _ =
                    tokio::time::timeout(Duration::from_secs(2), output_drained.changed()).await;
            }
            process::force_kill(pid);
            #[cfg(unix)]
            let signal = {
                use std::os::unix::process::ExitStatusExt;
                status.signal()
            };
            #[cfg(not(unix))]
            let signal = None;
            Ok::<_, anyhow::Error>((status.code().map(i64::from), signal, timed_out))
        };
        let ((exit_code, signal, timed_out), (stdout_bytes, stderr_bytes)) =
            tokio::try_join!(wait, output)?;
        // Remove the PID from daemon bookkeeping before releasing it to the OS.
        on_finished();
        child.reap().await?;
        let result = StreamResult {
            exit_code,
            signal,
            timed_out,
            duration_ms: crate::clock::elapsed_clock_ms()?.saturating_sub(start),
        };
        exit_sent.store(true, Ordering::Relaxed);
        tx.send(frame(&Data::StreamExit {
            result: result.clone(),
        })?)
        .await
        .context(ErrorCode::ConnectionClosed.error("stream writer stopped"))?;
        // Release the producer only after the result has been queued. The
        // receiver keeps the writer alive until the caller acknowledges it.
        drop(tx);
        Ok::<_, anyhow::Error>((result, stdout_bytes, stderr_bytes))
    };
    let (input_bytes, (result, stdout_bytes, stderr_bytes), ()) =
        tokio::try_join!(receive, completion, writer(socket_tx, rx))?;
    Ok(Outcome {
        result,
        input_bytes,
        stdout_bytes,
        stderr_bytes,
    })
}

pub(crate) async fn client(ws: &mut Ws) -> Result<StreamResult> {
    let (socket_tx, mut socket_rx) = ws.split();
    let (tx, rx) = mpsc::channel::<Message>(8);
    let (stop, mut stopped) = watch::channel(false);
    let (input_tx, mut input_rx) = mpsc::channel::<std::io::Result<Vec<u8>>>(8);
    // A dedicated thread may block in a terminal read. Unlike a Tokio blocking
    // task it does not prevent runtime shutdown; the CLI exits the process when
    // the remote result arrives. Bounded channels apply backpressure to pipes.
    std::thread::spawn(move || {
        use std::io::Read;
        let mut stdin = std::io::stdin().lock();
        loop {
            let mut bytes = vec![0; FILE_CHUNK];
            let result = stdin.read(&mut bytes).map(|n| {
                bytes.truncate(n);
                bytes
            });
            if result
                .as_ref()
                .is_err_and(|e| e.kind() == std::io::ErrorKind::Interrupted)
            {
                continue;
            }
            let end = !result.as_ref().is_ok_and(|bytes| !bytes.is_empty());
            if input_tx.blocking_send(result).is_err() || end {
                break;
            }
        }
    });
    let send_tx = tx.clone();
    let send = async move {
        loop {
            if *stopped.borrow() {
                return Ok::<_, anyhow::Error>(());
            }
            let bytes = tokio::select! {
                bytes=input_rx.recv()=>match bytes { Some(bytes)=>bytes?, None=>return Ok(()) },
                _=stopped.changed()=>return Ok(()),
            };
            let eof = bytes.is_empty();
            let message = if eof {
                frame(&Data::End)?
            } else {
                Message::Binary(bytes.into())
            };
            tokio::select! {
                r=send_tx.send(message)=>r.context(ErrorCode::ConnectionClosed.error("stream writer stopped"))?,
                _=stopped.changed()=>return Ok(()),
            }
            if eof {
                return Ok(());
            }
        }
    };
    let receive = async move {
        let mut stdout = tokio::io::stdout();
        let mut stderr = tokio::io::stderr();
        while let Some(message) = socket_rx.next().await {
            match message? {
                Message::Binary(bytes) if !bytes.is_empty() && bytes.len() <= FILE_CHUNK + 1 => {
                    match bytes[0] {
                        1 => {
                            stdout.write_all(&bytes[1..]).await?;
                            stdout.flush().await?;
                        }
                        2 => {
                            stderr.write_all(&bytes[1..]).await?;
                            stderr.flush().await?;
                        }
                        _ => bail!(ErrorCode::InvalidMessage.error("unknown output stream")),
                    }
                }
                Message::Text(text) => match serde_json::from_str::<Data>(&text)? {
                    Data::StreamExit { result } => {
                        let _ = stop.send(true);
                        let _ = tx.try_send(frame(&Data::StreamExitAck)?);
                        return Ok::<_, anyhow::Error>(result);
                    }
                    Data::Error { code, message } => {
                        bail!(crate::error::CodedError::from_wire(code, message))
                    }
                    _ => bail!(ErrorCode::InvalidMessage.error("expected stream result")),
                },
                Message::Ping(bytes) => tx
                    .send(Message::Pong(bytes))
                    .await
                    .context(ErrorCode::ConnectionClosed.error("stream writer stopped"))?,
                Message::Pong(_) => {}
                Message::Close(_) => break,
                _ => bail!(ErrorCode::InvalidMessage.error("invalid output chunk")),
            }
        }
        bail!(
            ErrorCode::ConnectionClosed
                .error("stream ended before its result; it will not be replayed")
        )
    };
    let transport = async {
        tokio::try_join!(send, writer(socket_tx, rx))?;
        Ok::<_, anyhow::Error>(())
    };
    tokio::pin!(receive, transport);
    tokio::select! {
        biased;
        result = &mut receive => {
            let result = result?;
            // The result and all output are already known. A lost final ACK
            // must not turn a successful result into a transport failure.
            let _ = tokio::time::timeout(Duration::from_secs(1), &mut transport).await;
            Ok(result)
        },
        result = &mut transport => { result?; receive.await },
    }
}
