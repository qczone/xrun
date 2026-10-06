mod common;
use anyhow::Result;
use std::{sync::Arc, time::Duration};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use xrun::testing::{config::ServerConfig, crypto};

#[test]
fn local_download_replaces_regular_files() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("output");
    xrun::testing::transfer::save_local(&path, b"first")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640))?;
    }
    xrun::testing::transfer::save_local(&path, b"second")?;
    assert_eq!(std::fs::read(&path)?, b"second");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&path)?.permissions().mode() & 0o777,
            0o640
        );
    }
    assert!(xrun::testing::transfer::save_local(temp.path(), b"bad").is_err());
    Ok(())
}

#[test]
fn local_download_rejects_existing_and_dangling_links() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let victim = temp.path().join("victim");
    std::fs::write(&victim, b"keep")?;
    for target in [&victim, &temp.path().join("missing")] {
        let link = temp.path().join("link");
        #[cfg(unix)]
        std::os::unix::fs::symlink(target, &link)?;
        #[cfg(windows)]
        std::os::windows::fs::symlink_file(target, &link)?;
        assert!(
            xrun::testing::transfer::save_local(&link, b"bad")
                .unwrap_err()
                .to_string()
                .contains("INVALID_PATH")
        );
        assert!(std::fs::symlink_metadata(&link)?.file_type().is_symlink());
        std::fs::remove_file(link)?;
    }
    assert_eq!(std::fs::read(victim)?, b"keep");
    assert!(!temp.path().join("missing").exists());
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn exited_leader_remains_waitable_until_group_cleanup() -> Result<()> {
    let mut child = xrun::testing::process::spawn(
        std::path::Path::new("/bin/sh"),
        &["-c".into(), "exit 7".into()],
        std::path::Path::new("/"),
        &Default::default(),
        "test",
        false,
    )?;
    assert_eq!(child.wait().await?.code(), Some(7));
    assert_eq!(child.wait().await?.code(), Some(7)); // WNOWAIT: the PID is still reserved.
    xrun::testing::process::terminate(child.pid);
    xrun::testing::process::force_kill(child.pid);
    child.reap().await?;
    let mut status = 0;
    assert_eq!(
        unsafe { libc::waitpid(child.pid as i32, &mut status, libc::WNOHANG) },
        -1
    );
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ECHILD)
    );
    Ok(())
}

struct Server(tokio::task::JoinHandle<Result<()>>);
impl Drop for Server {
    fn drop(&mut self) {
        self.0.abort();
    }
}

async fn anonymous_tls(
    port: u16,
    config: Arc<rustls::ClientConfig>,
) -> Result<tokio_rustls::client::TlsStream<tokio::net::TcpStream>> {
    let tcp = tokio::net::TcpStream::connect(("127.0.0.1", port)).await?;
    Ok(tokio_rustls::TlsConnector::from(config)
        .connect(rustls::pki_types::ServerName::try_from("localhost")?, tcp)
        .await?)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn anonymous_connection_limits_and_http_deadlines() -> Result<()> {
    common::library_logs()?;
    let temp = tempfile::tempdir()?;
    let port = std::net::TcpListener::bind("127.0.0.1:0")?
        .local_addr()?
        .port();
    let cfg = ServerConfig {
        port,
        addresses: vec![format!("127.0.0.1:{port}")],
        manual: true,
        no_detect: true,
        data_dir: temp.path().join("server"),
    };
    let keys = crypto::load_or_create_server(&cfg)?;
    let mut roots = rustls::RootCertStore::empty();
    roots.add(crypto::cert_der(&keys.ca_pem)?)?;
    let tls = Arc::new(
        rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth(),
    );
    let server = Server(tokio::spawn(xrun::testing::server::run(cfg)));
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match tokio::net::TcpStream::connect(("127.0.0.1", port)).await {
                Ok(tcp) => {
                    drop(tcp);
                    break;
                }
                Err(_) => tokio::time::sleep(Duration::from_millis(20)).await,
            }
        }
    })
    .await?;
    tokio::time::sleep(Duration::from_millis(100)).await;
    let mut held = Vec::new();
    for _ in 0..16 {
        held.push(anonymous_tls(port, tls.clone()).await?);
    }
    assert!(
        tokio::time::timeout(Duration::from_secs(2), anonymous_tls(port, tls.clone()))
            .await?
            .is_err()
    );
    drop(held);
    tokio::time::sleep(Duration::from_millis(200)).await;
    // Idle TLS, incomplete headers, and a slow body each have a finite lifetime.
    let mut idle = anonymous_tls(port, tls.clone()).await?;
    let mut headers = anonymous_tls(port, tls.clone()).await?;
    headers
        .write_all(b"POST /networks/unknown/roster HTTP/1.1\r\nHost: localhost\r\n")
        .await?;
    let mut body = anonymous_tls(port, tls.clone()).await?;
    body.write_all(format!("POST /networks/unknown/roster HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nX-Xrun-Version: {}\r\nContent-Length: 2000\r\n\r\n{{", xrun::protocol::VERSION).as_bytes()).await?;
    let closed = async {
        let mut bytes = Vec::new();
        let _ = idle.read_to_end(&mut bytes).await;
        bytes.clear();
        let _ = headers.read_to_end(&mut bytes).await;
        bytes.clear();
        let _ = body.read_to_end(&mut bytes).await;
    };
    tokio::time::timeout(Duration::from_secs(35), closed).await?;
    assert!(!server.0.is_finished());
    let mut healthy = anonymous_tls(port, tls).await?;
    healthy
        .write_all(
            format!(
                "GET /networks/unknown/roster HTTP/1.1\r\nHost: localhost\r\nX-Xrun-Version: {}\r\n\r\n",
                xrun::protocol::VERSION
            )
            .as_bytes(),
        )
        .await?;
    let mut response = Vec::new();
    tokio::time::timeout(Duration::from_secs(2), healthy.read_to_end(&mut response)).await??;
    assert!(String::from_utf8_lossy(&response).contains("404 Not Found"));
    Ok(())
}

#[cfg(target_os = "linux")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn accept_errors_do_not_stop_server() -> Result<()> {
    use std::process::Stdio;
    let temp = tempfile::tempdir()?;
    let port = std::net::TcpListener::bind("127.0.0.1:0")?
        .local_addr()?
        .port();
    let cfg = ServerConfig {
        port,
        addresses: vec![format!("127.0.0.1:{port}")],
        manual: true,
        no_detect: true,
        data_dir: temp.path().join(".xrun/server"),
    };
    let keys = crypto::load_or_create_server(&cfg)?;
    std::fs::write(
        temp.path().join(".xrun/config.toml"),
        toml::to_string(&cfg)?,
    )?;
    let mut cmd = tokio::process::Command::new(env!("CARGO_BIN_EXE_xrun"));
    cmd.env("HOME", temp.path())
        .env("RUST_LOG", "warn")
        .arg("server")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    unsafe {
        cmd.pre_exec(|| {
            let limit = libc::rlimit {
                rlim_cur: 64,
                rlim_max: 64,
            };
            if libc::setrlimit(libc::RLIMIT_NOFILE, &limit) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = cmd.spawn()?;
    let mut errors = child.stderr.take().unwrap();
    let logs = tokio::spawn(async move {
        let mut bytes = Vec::new();
        let _ = errors.read_to_end(&mut bytes).await;
        bytes
    });
    tokio::time::timeout(Duration::from_secs(5), async {
        while tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .is_err()
        {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await?;
    let mut held = Vec::new();
    for i in 1..=100 {
        let socket = tokio::net::TcpSocket::new_v4()?;
        socket.bind((std::net::Ipv4Addr::new(127, 0, 0, i), 0).into())?;
        held.push(
            socket
                .connect((std::net::Ipv4Addr::LOCALHOST, port).into())
                .await?,
        );
    }
    tokio::time::sleep(Duration::from_millis(700)).await;
    assert!(
        child.try_wait()?.is_none(),
        "Server exited on descriptor exhaustion"
    );
    drop(held);
    let client = crypto::http_client(&keys.ca_pem, None)?;
    let response = tokio::time::timeout(
        Duration::from_secs(5),
        client
            .get(format!("https://127.0.0.1:{port}/networks/unknown/roster"))
            .header("x-xrun-version", xrun::protocol::VERSION)
            .send(),
    )
    .await??;
    assert_eq!(response.status(), reqwest::StatusCode::NOT_FOUND);
    child.start_kill()?;
    child.wait().await?;
    let logs = logs.await?;
    assert!(
        String::from_utf8_lossy(&logs).contains("accept failed; retrying"),
        "{}",
        String::from_utf8_lossy(&logs)
    );
    Ok(())
}
