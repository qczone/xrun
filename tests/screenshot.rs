mod common;
use anyhow::{Context, Result, bail};
use common::*;
use xrun::testing::{
    crypto, membership::RosterCache, net, protocol::RelayMessage, protocol::*, secure,
};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn screenshot_download_checks_metadata_integrity_errors_and_local_destination() -> Result<()>
{
    tokio::time::timeout(std::time::Duration::from_secs(40), async {
        let mut lab = Lab::new().await?;
        stop_daemon(&lab.target, &mut lab.daemon).await?;
        let cache = RosterCache::open(&lab.target.join(".xrun/roster.db"))?;
        let network = &lab.target_identity.network.as_ref().unwrap().network_id;
        let roster = cache.load(network)?;
        let mut control = relay_socket(
            Some(&lab.target_identity),
            &roster,
            &format!("/networks/{network}/control"),
        )
        .await?;
        let RelayMessage::HelloAck { generation } = net::receive(&mut control).await? else {
            bail!("control acknowledgement")
        };
        let mut png = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut png, 2, 1);
            encoder.set_color(png::ColorType::Rgb);
            encoder.set_depth(png::BitDepth::Eight);
            encoder
                .write_header()?
                .write_image_data(&[255, 0, 0, 0, 255, 0])?;
        }
        let cases = [
            "file",
            "temporary",
            "checksum",
            "disconnect",
            "permission",
            "locked",
            "display",
        ];
        let server = async {
            for case in cases.into_iter().chain(cfg!(unix).then_some("symlink")) {
                let RelayMessage::Incoming { session_id } = net::receive(&mut control).await?
                else {
                    bail!("incoming")
                };
                let mut outer = net::websocket_at(
                    &roster.roster.relay_addresses[0],
                    &format!(
                        "/networks/{network}/attach/{}/{generation}/{session_id}",
                        lab.target_identity.device_id
                    ),
                    crypto::relay_tls_config(&roster.roster.relay_ca_pem)?,
                )
                .await?;
                assert!(matches!(
                    net::receive(&mut outer).await?,
                    RelayMessage::Connected { .. }
                ));
                let (mut ws, cert) = secure::server(outer, &lab.target_identity).await?;
                secure::exchange_server(&mut ws, &cache, network, &cert.context("certificate")?)
                    .await?;
                assert!(matches!(
                    net::receive(&mut ws).await?,
                    secure::Purpose::Execute
                ));
                net::send(
                    &mut ws,
                    &Data::Ready {
                        version: VERSION.into(),
                        protocol: xrun::protocol::ProtocolRange::CURRENT,
                        selected_protocol: 1,
                        device_id: lab.target_identity.device_id.clone(),
                        db_id: "screenshot-db".into(),
                        default_cwd: lab.target.to_string_lossy().into(),
                    },
                )
                .await?;
                assert!(matches!(
                    net::receive(&mut ws).await?,
                    Data::Request {
                        request: Request::Screenshot
                    }
                ));
                let error = match case {
                    "permission" => Some("PERMISSION_DENIED"),
                    "locked" => Some("SCREEN_LOCKED"),
                    "display" => Some("NO_DISPLAY"),
                    _ => None,
                };
                if let Some(code) = error {
                    net::send(
                        &mut ws,
                        &Data::Error {
                            code: code.into(),
                            message: "capture is unavailable".into(),
                        },
                    )
                    .await?;
                } else {
                    net::send(
                        &mut ws,
                        &Data::File {
                            path: String::new(),
                            size: png.len() as u64,
                            sha256: if case == "checksum" {
                                "0".repeat(64)
                            } else {
                                sha256(&png)
                            },
                            width: Some(2),
                            height: Some(1),
                            captured_at: Some("2026-10-06T00:00:00Z".into()),
                        },
                    )
                    .await?;
                    if case == "disconnect" {
                        use futures_util::SinkExt;
                        ws.send(tokio_tungstenite::tungstenite::Message::Binary(
                            png[..8].to_vec().into(),
                        ))
                        .await?;
                    } else {
                        net::send_bytes(&mut ws, &png).await?;
                        // A rejected checksum can close the connection before completion.
                        let _ = net::send(&mut ws, &Data::Complete).await;
                    }
                }
                let _ = ws.close(None).await;
            }
            Ok::<_, anyhow::Error>(())
        };
        let client = async {
            let path = lab.source.join("screen with spaces.png");
            let output = json(
                cli(
                    &lab.source,
                    &["target1", "screenshot", path.to_str().unwrap(), "--json"],
                )
                .await,
            );
            assert_eq!(output["width"], 2);
            assert_eq!(output["height"], 1);
            assert_eq!(output["device_id"], lab.target_identity.device_id);
            assert_eq!(output["captured_at"], "2026-10-06T00:00:00Z");
            assert_eq!(std::fs::read(&path)?, png);
            let output = json(cli(&lab.source, &["target1", "screenshot", "--json"]).await);
            let temporary =
                std::path::PathBuf::from(output["path"].as_str().context("temporary screenshot")?);
            assert!(
                temporary
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("xrun-screen-")
            );
            assert_eq!(std::fs::read(&temporary)?, png);
            std::fs::remove_file(temporary)?;
            for expected in [
                "CHECKSUM_MISMATCH",
                "CONNECTION_CLOSED",
                "PERMISSION_DENIED",
                "SCREEN_LOCKED",
                "NO_DISPLAY",
            ] {
                std::fs::write(&path, "existing file")?;
                let out = cli(
                    &lab.source,
                    &["target1", "screenshot", path.to_str().unwrap(), "--json"],
                )
                .await;
                assert!(!out.status.success());
                let error: serde_json::Value = serde_json::from_slice(&out.stderr)?;
                assert_eq!(error["code"], expected, "{error}");
                assert_eq!(std::fs::read(&path)?, b"existing file");
            }
            #[cfg(unix)]
            {
                let link = lab.source.join("linked.png");
                std::os::unix::fs::symlink(&path, &link)?;
                let out = cli(
                    &lab.source,
                    &["target1", "screenshot", link.to_str().unwrap(), "--json"],
                )
                .await;
                assert!(!out.status.success());
                let error: serde_json::Value = serde_json::from_slice(&out.stderr)?;
                assert_eq!(error["code"], "INVALID_PATH");
                assert!(std::fs::symlink_metadata(&link)?.file_type().is_symlink());
                assert_eq!(std::fs::read(&path)?, b"existing file");
            }
            Ok::<_, anyhow::Error>(())
        };
        tokio::try_join!(server, client)?;
        Ok::<_, anyhow::Error>(())
    })
    .await?
}

#[cfg(target_os = "linux")]
#[tokio::test]
#[ignore = "requires Xvfb with a 1024x768 24-bit X11 display"]
async fn x11_primary_display_png() {
    let capture = xrun::testing::screenshot::capture().await.unwrap();
    assert_eq!((capture.width, capture.height), (1024, 768));
    assert!(capture.at.ends_with('Z'));
    let decoder = png::Decoder::new(std::io::Cursor::new(capture.bytes));
    let mut reader = decoder.read_info().unwrap();
    let mut pixels = vec![0; reader.output_buffer_size().unwrap()];
    let info = reader.next_frame(&mut pixels).unwrap();
    assert_eq!(info.color_type, png::ColorType::Rgb);
    assert_eq!(&pixels[..3], &[0, 0, 0]);

    let mut lab = Lab::new().await.unwrap();
    let destination = lab.source.join("display.png");
    let output = json(
        cli(
            &lab.source,
            &[
                "target1",
                "screenshot",
                destination.to_str().unwrap(),
                "--json",
            ],
        )
        .await,
    );
    assert_eq!(output["width"], 1024);
    assert_eq!(output["height"], 768);
    let bytes = std::fs::read(destination).unwrap();
    let mut reader = png::Decoder::new(std::io::Cursor::new(bytes))
        .read_info()
        .unwrap();
    let mut pixels = vec![0; reader.output_buffer_size().unwrap()];
    reader.next_frame(&mut pixels).unwrap();
    assert_eq!(&pixels[..3], &[0, 0, 0]);
    stop_daemon(&lab.target, &mut lab.daemon).await.unwrap();
    stop_daemon(&lab.source, &mut lab.source_daemon)
        .await
        .unwrap();
}

#[cfg(target_os = "linux")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn headless_and_wayland_errors_reach_the_cli_without_creating_files() -> Result<()> {
    let mut lab = Lab::new().await?;
    for wayland in [false, true] {
        stop_daemon(&lab.target, &mut lab.daemon).await?;
        let mut command = logged(&lab.target, &["daemon"], "screenshot-target")?;
        command
            .env_remove("DISPLAY")
            .env_remove("WAYLAND_DISPLAY")
            .env_remove("XDG_SESSION_TYPE");
        if wayland {
            command.env("WAYLAND_DISPLAY", "wayland-test");
        }
        lab.daemon = command.spawn()?;
        online(&lab.source, "target1").await?;
        let path = lab.source.join("unavailable.png");
        let out = cli(
            &lab.source,
            &["target1", "screenshot", path.to_str().unwrap(), "--json"],
        )
        .await;
        assert!(!out.status.success());
        let error: serde_json::Value = serde_json::from_slice(&out.stderr)?;
        assert_eq!(
            error["code"],
            if wayland {
                "SCREENSHOT_UNAVAILABLE"
            } else {
                "NO_DISPLAY"
            }
        );
        assert!(!path.exists());
    }
    stop_daemon(&lab.target, &mut lab.daemon).await?;
    stop_daemon(&lab.source, &mut lab.source_daemon).await?;
    Ok(())
}
