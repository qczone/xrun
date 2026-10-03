mod common;
use anyhow::Result;
use common::*;
use std::{process::Stdio, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::{TcpListener, TcpStream},
    process::Child,
};

async fn forward(lab: &Lab, port: u16) -> Result<(Child, String)> {
    let mut child = logged(
        &lab.source,
        &["target1", "forward", &format!("0:{port}"), "--json"],
        "forward",
    )?
    .stdout(Stdio::piped())
    .spawn()?;
    let mut reader = BufReader::new(child.stdout.take().unwrap());
    let mut line = String::new();
    tokio::time::timeout(Duration::from_secs(5), reader.read_line(&mut line)).await??;
    let value: serde_json::Value = serde_json::from_str(&line)?;
    let address = value["local_address"].as_str().unwrap().to_string();
    assert!(address.starts_with("127.0.0.1:"));
    Ok((child, address))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn loopback_forwarding_handles_http_half_close_concurrency_and_pause() -> Result<()> {
    tokio::time::timeout(Duration::from_secs(40), async {
        let lab = Lab::new().await?;
        for invalid in ["0", "65536", "abc", "1:0", "1:2:3"] {
            assert_eq!(
                cli(&lab.source, &["target1", "forward", invalid])
                    .await
                    .status
                    .code(),
                Some(2)
            );
        }
        let backend = TcpListener::bind("127.0.0.1:0").await?;
        let (mut forwarding, address) = forward(&lab, backend.local_addr()?.port()).await?;
        let mut browser = TcpStream::connect(&address).await?;
        browser
            .write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .await?;
        let (mut service, _) = backend.accept().await?;
        let mut request = vec![0; 256];
        let n = service.read(&mut request).await?;
        assert!(request[..n].starts_with(b"GET / HTTP/1.1"));
        service
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nOK")
            .await?;
        service.shutdown().await?;
        browser.shutdown().await?;
        let mut response = vec![];
        browser.read_to_end(&mut response).await?;
        assert!(response.ends_with(b"\r\n\r\nOK"));
        drop(service);

        // Multiple connections, more than the bounded buffering, and a service
        // that waits for input EOF before producing a response.
        let content: Vec<u8> = (0..2_100_000).map(|i| (i % 256) as u8).collect();
        let remote = async {
            let mut workers = tokio::task::JoinSet::new();
            for round in 0..3 {
                let (mut tcp, _) = backend.accept().await?;
                let content = content.clone();
                workers.spawn(async move {
                    if round == 0 {
                        tokio::time::sleep(Duration::from_millis(100)).await;
                    }
                    let mut received = vec![];
                    tcp.read_to_end(&mut received).await?;
                    assert_eq!(received.len(), content.len());
                    assert_eq!(
                        xrun::protocol::sha256(&received),
                        xrun::protocol::sha256(&content)
                    );
                    tcp.write_all(&received).await?;
                    tcp.shutdown().await?;
                    Ok::<_, anyhow::Error>(())
                });
            }
            while let Some(result) = workers.join_next().await {
                result??;
            }
            Ok::<_, anyhow::Error>(())
        };
        let local = async {
            let mut workers = tokio::task::JoinSet::new();
            for _ in 0..3 {
                let mut client = TcpStream::connect(&address).await?;
                let content = content.clone();
                workers.spawn(async move {
                    client.write_all(&content).await?;
                    client.shutdown().await?;
                    let mut received = vec![];
                    client.read_to_end(&mut received).await?;
                    assert_eq!(received.len(), content.len());
                    assert_eq!(
                        xrun::protocol::sha256(&received),
                        xrun::protocol::sha256(&content)
                    );
                    Ok::<_, anyhow::Error>(())
                });
            }
            while let Some(result) = workers.join_next().await {
                result??;
            }
            Ok::<_, anyhow::Error>(())
        };
        tokio::try_join!(remote, local)?;

        // Both peers send beyond TCP buffer capacity while reading concurrently.
        // This catches a relay that blocks the reverse direction on a slow sink.
        let client = TcpStream::connect(&address).await?;
        let (service, _) = backend.accept().await?;
        let (mut client_read, mut client_write) = client.into_split();
        let (mut service_read, mut service_write) = service.into_split();
        let bulk = vec![0x5a; 5_000_000];
        let upload = async {
            client_write.write_all(&bulk).await?;
            client_write.shutdown().await?;
            Ok::<_, anyhow::Error>(())
        };
        let download = async {
            service_write.write_all(&bulk).await?;
            service_write.shutdown().await?;
            Ok::<_, anyhow::Error>(())
        };
        let receive_upload = async {
            let mut bytes = vec![];
            service_read.read_to_end(&mut bytes).await?;
            assert_eq!(bytes.len(), bulk.len());
            assert_eq!(
                xrun::protocol::sha256(&bytes),
                xrun::protocol::sha256(&bulk)
            );
            Ok::<_, anyhow::Error>(())
        };
        let receive_download = async {
            tokio::time::sleep(Duration::from_millis(100)).await;
            let mut bytes = vec![];
            client_read.read_to_end(&mut bytes).await?;
            assert_eq!(bytes.len(), bulk.len());
            assert_eq!(
                xrun::protocol::sha256(&bytes),
                xrun::protocol::sha256(&bulk)
            );
            Ok::<_, anyhow::Error>(())
        };
        tokio::try_join!(upload, download, receive_upload, receive_download)?;

        let mut active = TcpStream::connect(&address).await?;
        let (mut service, _) = backend.accept().await?;
        service.write_all(b"connected").await?;
        let mut bytes = [0; 9];
        active.read_exact(&mut bytes).await?;
        ok(cli(&lab.target, &["daemon", "pause"]).await);
        let mut bytes = vec![];
        let _ = active.read_to_end(&mut bytes).await;
        ok(cli(&lab.target, &["daemon", "resume"]).await);
        let mut active = TcpStream::connect(&address).await?;
        let (mut service, _) = backend.accept().await?;
        service.write_all(b"resumed").await?;
        let mut bytes = [0; 7];
        active.read_exact(&mut bytes).await?;
        assert_eq!(&bytes, b"resumed");
        ok(cli(&lab.source, &["revoke", "target1"]).await);
        let mut bytes = vec![];
        let _ = active.read_to_end(&mut bytes).await;
        forwarding.start_kill()?;
        forwarding.wait().await?;

        let db = rusqlite::Connection::open(lab.target.join(".xrun/daemon.db"))?;
        let count: i64 = db.query_row(
            "SELECT count(*) FROM audit WHERE json_extract(data,'$.op')='forward'",
            [],
            |r| r.get(0),
        )?;
        assert!(count >= 4);
        Ok::<_, anyhow::Error>(())
    })
    .await??;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn forwarding_falls_back_to_ipv6_loopback() -> Result<()> {
    tokio::time::timeout(Duration::from_secs(20), async {
        let lab = Lab::new().await?;
        let backend = TcpListener::bind("[::1]:0").await?;
        let (mut forwarding, address) = forward(&lab, backend.local_addr()?.port()).await?;
        let mut client = TcpStream::connect(address).await?;
        let (mut service, _) = backend.accept().await?;
        service.write_all(b"ipv6").await?;
        service.shutdown().await?;
        client.shutdown().await?;
        let mut received = vec![];
        client.read_to_end(&mut received).await?;
        assert_eq!(received, b"ipv6");
        forwarding.start_kill()?;
        forwarding.wait().await?;
        Ok::<_, anyhow::Error>(())
    })
    .await??;
    Ok(())
}
