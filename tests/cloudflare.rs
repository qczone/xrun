mod common;
use anyhow::{Context, Result};
use common::*;
use std::{
    path::{Path, PathBuf},
    process::{Output, Stdio},
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    process::Child,
};
use xrun::{
    config::Identity,
    membership::RosterCache,
    protocol::{MAX_FILE, VERSION, sha256},
    relay::{Proof, RelayMessage},
};

struct CloudLab {
    root: tempfile::TempDir,
    source: PathBuf,
    target: PathBuf,
    manager: Child,
    member: Child,
}
impl Drop for CloudLab {
    fn drop(&mut self) {
        let _ = self.manager.start_kill();
        let _ = self.member.start_kill();
    }
}
async fn run(home: &Path, args: &[&str]) -> Output {
    tokio::time::timeout(
        Duration::from_secs(180),
        command(home, args).env("RUST_LOG", "xrun=debug").output(),
    )
    .await
    .expect("Cloudflare CLI timeout")
    .unwrap()
}
async fn cloud_online(home: &Path, name: &str) -> Result<()> {
    tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            let output = run(home, &[name, "info", "--json"]).await;
            if output.status.success()
                && serde_json::from_slice::<serde_json::Value>(&output.stdout)
                    .is_ok_and(|v| v["online"] == true)
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    })
    .await
    .context("Cloudflare device did not connect")
}
async fn lab() -> Result<CloudLab> {
    let file = std::env::var("XRUN_TEST_CF_LINK_FILE")
        .context("Set XRUN_TEST_CF_LINK_FILE to a private file containing the relay URL")?;
    let link = std::fs::read_to_string(file)?;
    let root = tempfile::tempdir()?;
    let source = root.path().join("source");
    let target = root.path().join("target");
    std::fs::create_dir_all(&source)?;
    std::fs::create_dir_all(&target)?;
    ok(run(
        &source,
        &[
            "up",
            "--relay",
            link.trim(),
            "--name",
            "source1",
            "--no-daemon",
        ],
    )
    .await);
    let manager = logged(&source, &["daemon"], "cf-manager")?.spawn()?;
    cloud_online(&source, "source1").await?;
    let invite = json(run(&source, &["invite", "--allow", "--json"]).await);
    ok(run(
        &target,
        &[
            "join",
            invite["link"].as_str().context("invite")?,
            "--name",
            "target1",
            "--no-daemon",
        ],
    )
    .await);
    let member = logged(&target, &["daemon"], "cf-member")?.spawn()?;
    let lab = CloudLab {
        root,
        source,
        target,
        manager,
        member,
    };
    if let Err(error) = cloud_online(&lab.source, "target1").await {
        let response = run(&lab.source, &["target1", "info", "--json"]).await;
        anyhow::bail!(
            "{error}: {} {}",
            String::from_utf8_lossy(&response.stdout),
            String::from_utf8_lossy(&response.stderr)
        );
    }
    Ok(lab)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "run via cloudflare test:interop, or set XRUN_TEST_CF_LINK_FILE for a deployed relay"]
async fn public_relay_executes_transfers_streams_and_rejects_route_takeover() -> Result<()> {
    tokio::time::timeout(Duration::from_secs(600), async {
        let lab = lab().await?;
        let binary = binary().to_string_lossy().into_owned();
        let started = Instant::now();
        assert_eq!(ok(run(&lab.source, &["target1", "--", &binary, "--version"]).await).trim(), format!("xrun {VERSION}"));
        println!("short command: {:.3}s", started.elapsed().as_secs_f64());
        let job = json(run(&lab.source, &["target1", "start", "--json", "--", &binary, "--version"]).await);
        let job_id = job["job_id"].as_str().context("job ID")?;
        let completed = json(run(&lab.source, &["target1", "wait", job_id, "--json"]).await);
        assert_eq!(completed["job"]["exit_code"], 0);
        assert!(ok(run(&lab.source, &["target1", "logs", job_id]).await).contains(VERSION));
        println!("network creation, pairing, execution and job history: passed");
        let source: Identity = xrun::config::read(&lab.source.join(".xrun/identity.toml"))?;
        let target: Identity = xrun::config::read(&lab.target.join(".xrun/identity.toml"))?;
        let network = &source.network.as_ref().context("network")?.network_id;
        let roster = RosterCache::open(&lab.source.join(".xrun/roster.db"))?.load(network)?;
        let path = format!("/networks/{network}/control");
        let mut attacker = xrun::net::websocket_at(&roster.roster.relay_addresses[0], &path, xrun::crypto::relay_tls_config(&roster.roster.relay_ca_pem)?).await?;
        let RelayMessage::Challenge { nonce } = xrun::net::receive(&mut attacker).await? else { anyhow::bail!("challenge") };
        let mut proof = Proof::create(&source, network, &path, &nonce, None)?;
        proof.device_id = target.device_id.clone();
        xrun::net::send(&mut attacker, &RelayMessage::Authenticate { proof: Some(proof) }).await?;
        assert!(matches!(xrun::net::receive(&mut attacker).await?, RelayMessage::Error { code, .. } if code == "UNAUTHENTICATED"));
        let connect = format!("/networks/{network}/connect/{}", target.device_id);
        let mut anonymous = xrun::net::websocket_at(&roster.roster.relay_addresses[0], &connect, xrun::crypto::relay_tls_config(&roster.roster.relay_ca_pem)?).await?;
        xrun::network::authenticate(&mut anonymous, None, network, &connect, None).await?;
        assert!(matches!(xrun::net::receive(&mut anonymous).await?, RelayMessage::Error { code, .. } if code == "UNAUTHENTICATED"));
        cloud_online(&lab.source, "target1").await?;
        println!("certificate interoperability and route takeover rejection: passed");
        // Exercise the actual product file limit, rather than a small fixture.
        let content: Vec<u8> = (0..MAX_FILE).map(|i| (i % 251) as u8).collect();
        let local = lab.root.path().join("upload.bin"); let remote = lab.target.join("artifact.bin"); let download = lab.root.path().join("download.bin");
        std::fs::write(&local, &content)?;
        println!("starting 64 MiB upload");
        let started = Instant::now();
        ok(run(&lab.source, &["target1", "push", &local.to_string_lossy(), &remote.to_string_lossy(), "--no-overwrite"]).await);
        println!("64 MiB upload: {:.3}s; starting download", started.elapsed().as_secs_f64());
        let started = Instant::now();
        let result = json(run(&lab.source, &["target1", "pull", &remote.to_string_lossy(), &download.to_string_lossy(), "--json"]).await);
        println!("64 MiB download: {:.3}s", started.elapsed().as_secs_f64());
        assert_eq!(result["sha256"], sha256(&content));
        assert_eq!(sha256(&std::fs::read(download)?), sha256(&content));
        println!("64 MiB file transfer in both directions: passed");
        let backend = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let port = backend.local_addr()?.port();
        let echo = tokio::spawn(async move {
            let (mut stream, _) = backend.accept().await?;
            let mut bytes = vec![];
            stream.read_to_end(&mut bytes).await?;
            stream.write_all(&bytes).await?;
            stream.shutdown().await?;
            Ok::<_, anyhow::Error>(())
        });
        let mut forwarding = logged(&lab.source, &["target1", "forward", &format!("0:{port}"), "--json"], "cf-forward")?
            .stdout(Stdio::piped()).spawn()?;
        let mut reader = BufReader::new(forwarding.stdout.take().unwrap());
        let mut line = String::new();
        tokio::time::timeout(Duration::from_secs(10), reader.read_line(&mut line)).await??;
        let address: serde_json::Value = serde_json::from_str(&line)?;
        let mut browser = tokio::net::TcpStream::connect(address["local_address"].as_str().context("forward address")?).await?;
        browser.write_all(&content[..2_000_000]).await?;
        browser.shutdown().await?;
        let mut echoed = vec![];
        browser.read_to_end(&mut echoed).await?;
        assert_eq!(sha256(&echoed), sha256(&content[..2_000_000]));
        echo.await??;
        forwarding.start_kill()?;
        forwarding.wait().await?;
        println!("TCP forwarding and half-close: passed");
        #[cfg(unix)] {
            let bytes = &content[..2_000_000];
            let mut child = command(&lab.source, &["target1", "-i", "--", "cat"]).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()?;
            let mut stdin = child.stdin.take().unwrap();
            let send = async { stdin.write_all(bytes).await?; drop(stdin); Ok::<_, anyhow::Error>(()) };
            let receive = async { Ok::<_, anyhow::Error>(child.wait_with_output().await?) };
            let ((), output) = tokio::try_join!(send, receive)?;
            assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
            assert_eq!(sha256(&output.stdout), sha256(bytes));
            println!("binary streaming: passed");
        }
        Ok::<_, anyhow::Error>(())
    }).await?
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "run via cloudflare test:interop, or set XRUN_TEST_CF_LINK_FILE for a deployed relay"]
async fn public_relay_resumes_idle_connections_and_propagates_revocation() -> Result<()> {
    tokio::time::timeout(Duration::from_secs(240), async {
        let mut lab = lab().await?;
        let binary = binary().to_string_lossy().into_owned();
        // The manager's CLI retains its identity while its daemon is offline.
        stop_daemon(&lab.source, &mut lab.manager).await?;
        assert_eq!(
            ok(run(&lab.source, &["target1", "--", &binary, "--version"]).await).trim(),
            format!("xrun {VERSION}")
        );
        println!("manager-offline execution: passed");
        let mut restarted = logged(&lab.source, &["daemon"], "cf-manager-restart")?.spawn()?;
        cloud_online(&lab.source, "source1").await?;
        // Leave only idle control sockets; the implementation has no JS heartbeat.
        tokio::time::sleep(Duration::from_secs(45)).await;
        assert_eq!(
            ok(run(&lab.source, &["target1", "--", &binary, "--version"]).await).trim(),
            format!("xrun {VERSION}")
        );
        ok(run(&lab.source, &["revoke", "target1", "--json"]).await);
        let refused = run(&lab.source, &["target1", "--", &binary, "--version"]).await;
        assert!(String::from_utf8_lossy(&refused.stderr).contains("DEVICE_REVOKED"));
        let revoked_member = run(&lab.target, &["status", "--json"]).await;
        assert!(!revoked_member.status.success());
        let status: serde_json::Value = serde_json::from_slice(&revoked_member.stdout)?;
        assert_eq!(status["server_error"]["code"], "DEVICE_REVOKED");
        stop_daemon(&lab.source, &mut restarted).await?;
        println!("idle resume and revocation: passed");
        Ok::<_, anyhow::Error>(())
    })
    .await?
}
