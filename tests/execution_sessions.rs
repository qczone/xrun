mod common;
use anyhow::{Context, Result, bail};
use common::*;
use std::{process::Stdio, time::Duration};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
use xrun::{crypto, membership::RosterCache, net, protocol::*, relay::RelayMessage, secure};

fn job(execution: &Execution, lab: &Lab) -> Job {
    Job {
        job_id: "ABC123".into(),
        request_id: execution.request_id.clone(),
        request_hash: execution.hash(),
        source_device_id: lab.source_identity.device_id.clone(),
        target_device_id: lab.target_identity.device_id.clone(),
        db_id: execution.db_id.clone(),
        program: execution.program.clone(),
        args: execution.args.clone(),
        cwd: execution.cwd.clone(),
        state: JobState::Running,
        exit_code: None,
        signal: None,
        duration_ms: None,
        last_seq: 0,
        output_complete: true,
        incomplete_reason: None,
        error: None,
        created_at_ms: now_ms(),
        updated_at_ms: now_ms(),
        leftover_possible: false,
        process: None,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn foreground_uses_one_session_and_recovers_without_resubmitting() -> Result<()> {
    tokio::time::timeout(Duration::from_secs(30), async {
        let mut lab = Lab::new().await?;
        stop_daemon(&lab.source, &mut lab.source_daemon).await?;
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
        let peer = async {
            // Normal execution has exactly one session. Recovery opens only
            // queries: losing either the acknowledgement or logs never resubmits.
            for failure in ["none", "ack", "logs"] {
                let mut accepted = None;
                let steps: &[&str] = match failure {
                    "ack" => &["exec", "jobs", "logs"],
                    "logs" => &["exec", "logs"],
                    _ => &["exec"],
                };
                for step in steps {
                    let RelayMessage::Incoming { session_id } = net::receive(&mut control).await?
                    else {
                        bail!("incoming session")
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
                    secure::exchange_server(
                        &mut ws,
                        &cache,
                        network,
                        &cert.context("certificate")?,
                    )
                    .await?;
                    assert!(matches!(
                        net::receive(&mut ws).await?,
                        secure::Purpose::Execute
                    ));
                    net::send(
                        &mut ws,
                        &Data::Ready {
                            version: VERSION.into(),
                            device_id: lab.target_identity.device_id.clone(),
                            db_id: "test-db".into(),
                            default_cwd: lab.target.to_string_lossy().into(),
                        },
                    )
                    .await?;
                    let Data::Request { request } = net::receive(&mut ws).await? else {
                        bail!("request")
                    };
                    let after = match (step, request) {
                        (&"exec", Request::Exec { execution, follow }) => {
                            assert!(follow);
                            assert_eq!(execution.request_id, failure);
                            net::receive_bytes(
                                &mut ws,
                                execution.input_size,
                                &execution.input_sha256,
                                MAX_INPUT as u64,
                            )
                            .await?;
                            let value = job(&execution, &lab);
                            accepted = Some(value.clone());
                            if failure == "ack" {
                                ws.close(None).await?;
                                continue;
                            }
                            net::send(&mut ws, &Data::Job { job: value }).await?;
                            0
                        }
                        (&"jobs", Request::Jobs { request_id, .. }) => {
                            assert_eq!(request_id.as_deref(), Some(failure));
                            net::send(
                                &mut ws,
                                &Data::Jobs {
                                    jobs: vec![accepted.clone().unwrap()],
                                },
                            )
                            .await?;
                            net::close(&mut ws).await;
                            continue;
                        }
                        (&"logs", Request::Logs { id, after, follow }) => {
                            assert_eq!(id, "ABC123");
                            assert!(follow);
                            assert_eq!(after, if failure == "logs" { 1 } else { 0 });
                            after
                        }
                        (_, request) => bail!("unexpected request at {step}: {request:?}"),
                    };
                    let mut value = accepted.clone().unwrap();
                    if after == 0 {
                        value.last_seq = 1;
                        net::send(
                            &mut ws,
                            &Data::Logs {
                                events: vec![LogEvent {
                                    seq: 1,
                                    stream: "stdout".into(),
                                    data_base64: "YmVmb3JlCg==".into(),
                                }],
                                job: value.clone(),
                            },
                        )
                        .await?;
                    }
                    if failure == "logs" && *step == "exec" {
                        ws.close(None).await?;
                        continue;
                    }
                    value.last_seq = 2;
                    value.state = JobState::Exited;
                    value.exit_code = Some(7);
                    net::send(
                        &mut ws,
                        &Data::Logs {
                            events: vec![LogEvent {
                                seq: 2,
                                stream: "stderr".into(),
                                data_base64: "YWZ0ZXIK".into(),
                            }],
                            job: value,
                        },
                    )
                    .await?;
                    net::send(&mut ws, &Data::End).await?;
                    net::close(&mut ws).await;
                }
            }
            Ok::<_, anyhow::Error>(())
        };
        let commands = async {
            for failure in ["none", "ack", "logs"] {
                let output = cli(
                    &lab.source,
                    &["target1", "--request-id", failure, "--", "test-command"],
                )
                .await;
                assert_eq!(
                    output.status.code(),
                    Some(7),
                    "{}",
                    String::from_utf8_lossy(&output.stderr)
                );
                assert_eq!(output.stdout, b"before\n");
                assert_eq!(output.stderr, b"after\n");
            }
            Ok::<_, anyhow::Error>(())
        };
        tokio::try_join!(peer, commands)?;
        Ok::<_, anyhow::Error>(())
    })
    .await?
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn execution_survives_log_disconnect_and_follow_does_not_change_request_identity()
-> Result<()> {
    tokio::time::timeout(Duration::from_secs(40), async {
        let mut lab = Lab::new().await?;
        let source = lab.root.path().join("job-child.rs");
        std::fs::write(&source, r#"
use std::{io::Write, time::Duration};
fn main() {
    let args: Vec<String> = std::env::args().collect();
    std::fs::OpenOptions::new().create(true).append(true).open(&args[1]).unwrap().write_all(b"x").unwrap();
    println!("before"); std::io::stdout().flush().unwrap();
    while !std::path::Path::new(&args[2]).exists() { std::thread::sleep(Duration::from_millis(10)); }
    println!("after"); std::process::exit(9);
}"#)?;
        let executable = lab.root.path().join(if cfg!(windows) { "job-child.exe" } else { "job-child" });
        let output = tokio::process::Command::new("rustc").arg(source).arg("-o").arg(&executable).output().await?;
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        let effect = lab.target.join("effect"); let finish = lab.target.join("finish");
        let args = ["target1", "--request-id", "resume", "--timeout", "0", "--", executable.to_str().unwrap(), effect.to_str().unwrap(), finish.to_str().unwrap()];
        let mut child = command(&lab.source, &args).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()?;
        let mut reader = BufReader::new(child.stdout.take().unwrap());
        let mut line = String::new(); reader.read_line(&mut line).await?;
        assert_eq!(line, "before\n");
        lab.relay.stop().await?;
        std::fs::write(&finish, b"done")?;
        lab.relay.start().await?;
        let mut rest = String::new(); reader.read_to_string(&mut rest).await?;
        assert_eq!(rest, "after\n");
        assert_eq!(child.wait_with_output().await?.status.code(), Some(9));
        let mut background = args.to_vec(); background.insert(1, "start"); background.insert(2, "--json");
        let repeated = json(cli(&lab.source, &background).await);
        assert_eq!(repeated["exit_code"], 9);
        let output = cli(&lab.source, &args).await;
        assert_eq!(output.status.code(), Some(9));
        assert_eq!(output.stdout, b"before\nafter\n");
        assert_eq!(std::fs::read(&effect)?, b"x");
        Ok::<_, anyhow::Error>(())
    }).await?
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pipelined_purpose_never_sends_execution_before_handshake_acceptance() -> Result<()> {
    use xrun::membership::Manager;
    tokio::time::timeout(Duration::from_secs(20), async {
        let mut lab = Lab::new().await?;
        stop_daemon(&lab.source, &mut lab.source_daemon).await?;
        stop_daemon(&lab.target, &mut lab.daemon).await?;
        let network = &lab.target_identity.network.as_ref().unwrap().network_id;
        let roster = RosterCache::open(&lab.target.join(".xrun/roster.db"))?.load(network)?;
        let mut control = relay_socket(Some(&lab.target_identity), &roster, &format!("/networks/{network}/control")).await?;
        let RelayMessage::HelloAck { generation } = net::receive(&mut control).await? else { bail!("control acknowledgement") };
        let failures = ["version", "signature", "ready-version", "ready-identity", "denied", "revoked"];
        let peer = async {
            for failure in failures {
                let RelayMessage::Incoming { session_id } = net::receive(&mut control).await? else { bail!("incoming session") };
                let mut outer = net::websocket_at(&roster.roster.relay_addresses[0], &format!("/networks/{network}/attach/{}/{generation}/{session_id}", lab.target_identity.device_id), crypto::relay_tls_config(&roster.roster.relay_ca_pem)?).await?;
                assert!(matches!(net::receive(&mut outer).await?, RelayMessage::Connected { .. }));
                let (mut ws, cert) = secure::server(outer, &lab.target_identity).await?;
                roster.peer(&cert.context("certificate")?, Some(&lab.source_identity.device_id))?;
                let exchange: serde_json::Value = net::receive(&mut ws).await?;
                assert_eq!(exchange["version"], VERSION);
                // Receiving Purpose before sending our roster proves there is
                // no extra round trip. It must be the only pipelined message.
                assert!(matches!(net::receive(&mut ws).await?, secure::Purpose::Execute));
                assert!(tokio::time::timeout(Duration::from_millis(50), net::receive::<Data>(&mut ws)).await.is_err());
                let mut sent_roster = roster.clone();
                if failure == "signature" { sent_roster.roster.version += 1; }
                if failure == "revoked" {
                    sent_roster = Manager::open(&lab.source.join(".xrun/manager"))?.revoke("target1")?;
                }
                net::send(&mut ws, &serde_json::json!({ "version": if failure == "version" { "incompatible" } else { VERSION }, "roster": sent_roster })).await?;
                if failure.starts_with("ready-") || failure == "denied" {
                    // Even a valid roster does not permit sending Exec before Ready.
                    assert!(tokio::time::timeout(Duration::from_millis(50), net::receive::<Data>(&mut ws)).await.is_err());
                    let response = if failure == "denied" {
                        Data::Error { code: "PERMISSION_DENIED".into(), message: "not allowed".into() }
                    } else {
                        Data::Ready { version: if failure == "ready-version" { "incompatible" } else { VERSION }.into(), device_id: if failure == "ready-identity" { lab.source_identity.device_id.clone() } else { lab.target_identity.device_id.clone() }, db_id: "test-db".into(), default_cwd: lab.target.to_string_lossy().into() }
                    };
                    net::send(&mut ws, &response).await?;
                }
                assert!(net::receive::<Data>(&mut ws).await.is_err(), "{failure} must not send an execution request");
            }
            Ok::<_, anyhow::Error>(())
        };
        let commands = async {
            for failure in failures {
                let output = cli(&lab.source, &["target1", "--request-id", failure, "--", "must-not-run"]).await;
                assert_eq!(output.status.code(), Some(125), "{failure}: {}", String::from_utf8_lossy(&output.stderr));
            }
            assert!(json(cli(&lab.source, &["recent", "--json"]).await).as_array().unwrap().is_empty());
            Ok::<_, anyhow::Error>(())
        };
        tokio::try_join!(peer, commands)?;
        Ok::<_, anyhow::Error>(())
    }).await?
}
